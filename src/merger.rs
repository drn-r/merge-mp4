use std::error::Error;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use scuffle_mp4::header::{BoxHeader, FullBoxHeader};
use scuffle_mp4::types::{
    ctts::{Ctts, CttsEntry},
    edts::Edts,
    elst::{Elst, ElstEntry},
    moov::Moov,
    stbl::Stbl,
    stss::Stss,
    stsz::Stsz,
    trak::Trak,
};
use scuffle_mp4::{BoxType, DynBox};

use crate::scanner::file_name;

struct Input {
    ftyp: DynBox,
    moov: Moov,
    media: Vec<Range<u64>>,
    offsets: Vec<Vec<u64>>,
}

pub fn merge(files: &[PathBuf], output: &Path, overwrite: bool) -> Result<(), Box<dyn Error>> {
    if files.is_empty() {
        return Err("没有可合并的输入文件".into());
    }
    if output.exists() && !overwrite {
        return Err(format!("输出文件已存在，不会覆盖: {}", output.display()).into());
    }
    let mut inputs = Vec::new();
    for path in files {
        let input =
            read_input(path).map_err(|error| format!("{} 无法合并: {error}", path.display()))?;
        validate_input(&input).map_err(|error| format!("{} 无法合并: {error}", path.display()))?;
        inputs.push(input);
    }
    let mut movie = inputs[0].moov.clone();
    let track_count = movie.traks.len();
    let mut offsets = vec![Vec::new(); track_count];
    // 各轨道统一到所有片段媒体时间基的最小公倍数，重定时精确无损。
    let mut target_timescales = vec![1u32; track_count];
    for (path, input) in files.iter().zip(&inputs) {
        if input.moov.traks.len() != track_count {
            return Err(format!("{} 的轨道数量与第一个文件不一致", path.display()).into());
        }
        for (i, track) in input.moov.traks.iter().enumerate() {
            target_timescales[i] = lcm(target_timescales[i], track.mdia.mdhd.timescale)?;
        }
    }
    // 只保留首个片段的初始偏移，整条轨道用单条 edit list，兼容 Windows 播放器/缩略图。
    let initial_edit_offsets: Vec<i64> = movie
        .traks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let factor = i64::from(target_timescales[i] / track.mdia.mdhd.timescale);
            initial_edit_offset(track) * factor
        })
        .collect();
    for (i, track) in movie.traks.iter_mut().enumerate() {
        clear_track(track);
        track.mdia.mdhd.timescale = target_timescales[i];
    }
    movie.mvhd.duration = 0;
    movie.mvhd.header.version = 1;
    movie.unknown.clear();
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut destination = tempfile::NamedTempFile::new_in(parent)?;
    inputs[0].ftyp.mux(&mut destination)?;
    let media_start = destination.stream_position()?;
    destination.write_all(&1u32.to_be_bytes())?;
    destination.write_all(b"mdat")?;
    destination.write_all(&0u64.to_be_bytes())?;

    for (index, (path, input)) in files.iter().zip(&inputs).enumerate() {
        println!("[{}/{}] 合并 {}", index + 1, files.len(), file_name(path));
        if input.moov.mvhd.timescale != movie.mvhd.timescale
            || input.moov.traks.len() != movie.traks.len()
        {
            return Err(format!("{} 的容器时间基或轨道数量不一致", path.display()).into());
        }
        let mut reader = BufReader::new(File::open(path)?);
        let mut media_locations = Vec::new();
        for range in &input.media {
            media_locations.push(destination.stream_position()?);
            reader.seek(SeekFrom::Start(range.start))?;
            let length = range.end - range.start;
            if io::copy(&mut reader.by_ref().take(length), &mut destination)? != length {
                return Err(format!("{} 的媒体数据不完整", path.display()).into());
            }
        }
        for (track_index, source) in input.moov.traks.iter().enumerate() {
            let relocated = input.offsets[track_index]
                .iter()
                .map(|offset| {
                    let index = input
                        .media
                        .iter()
                        .position(|range| range.contains(offset))
                        .ok_or_else(|| invalid("存在指向 mdat 之外的块偏移"))?;
                    Ok(media_locations[index] + offset - input.media[index].start)
                })
                .collect::<io::Result<Vec<_>>>()?;
            let mut source = source.clone();
            rescale_timing(&mut source, target_timescales[track_index])?;
            append_track(
                &mut movie.traks[track_index],
                &source,
                input.moov.mvhd.duration,
                &mut offsets[track_index],
                &relocated,
            )
            .map_err(|error| {
                format!(
                    "{} 的轨道 {}: {error}",
                    path.display(),
                    source.tkhd.track_id
                )
            })?;
        }
        movie.mvhd.duration = movie
            .mvhd
            .duration
            .checked_add(input.moov.mvhd.duration)
            .ok_or("视频总时长溢出")?;
    }
    let media_end = destination.stream_position()?;
    destination.seek(SeekFrom::Start(media_start + 8))?;
    destination.write_all(&(media_end - media_start).to_be_bytes())?;
    destination.seek(SeekFrom::Start(media_end))?;
    for (index, track) in movie.traks.iter_mut().enumerate() {
        let composition = track.mdia.minf.stbl.ctts.as_mut().unwrap();
        let signed = composition
            .entries
            .iter()
            .any(|entry| entry.sample_offset < 0);
        if composition.entries.iter().any(|entry| {
            if signed {
                i32::try_from(entry.sample_offset).is_err()
            } else {
                u32::try_from(entry.sample_offset).is_err()
            }
        }) {
            return Err(invalid("PTS 偏移超过 MP4 表的取值范围").into());
        }
        composition.header.version = u8::from(signed);
        let segment_duration = track.tkhd.duration;
        let media_time = initial_edit_offsets[index].min(track.mdia.mdhd.duration as i64);
        track.edts.as_mut().unwrap().elst.as_mut().unwrap().entries = vec![ElstEntry {
            segment_duration,
            media_time,
            media_rate_integer: 1,
            media_rate_fraction: 0,
        }];
    }
    let mut metadata = Vec::new();
    movie.mux(&mut metadata)?;
    let mut table_index = 0;
    let metadata = rewrite_offsets(Bytes::from(metadata), &mut offsets, &mut table_index, true)?;
    if table_index != offsets.len() {
        return Err(invalid("输出轨道块偏移数量不一致").into());
    }
    destination.write_all(&metadata)?;
    destination.flush()?;
    destination.as_file().sync_all()?;
    if overwrite {
        destination.persist(output)?;
    } else {
        destination.persist_noclobber(output)?;
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_header(reader: &mut impl Read, remaining: u64) -> io::Result<([u8; 4], u64, u64)> {
    if remaining < 8 {
        return Err(invalid("MP4 box header 不完整"));
    }
    let mut header = [0; 8];
    reader.read_exact(&mut header)?;
    let short_size = u32::from_be_bytes(header[..4].try_into().unwrap());
    let mut header_size = 8;
    let size = match short_size {
        0 => remaining,
        1 => {
            if remaining < 16 {
                return Err(invalid("扩展 MP4 box header 不完整"));
            }
            header_size = 16;
            let mut size = [0; 8];
            reader.read_exact(&mut size)?;
            u64::from_be_bytes(size)
        }
        size => u64::from(size),
    };
    if size < header_size || size > remaining {
        return Err(invalid("MP4 box 大小无效"));
    }
    Ok((header[4..].try_into().unwrap(), size, header_size))
}

fn read_input(path: &Path) -> io::Result<Input> {
    let mut reader = BufReader::new(File::open(path)?);
    let length = reader.get_ref().metadata()?.len();
    let mut ftyp = None;
    let mut moov = None;
    let mut media = Vec::new();
    let mut offsets = Vec::new();
    while reader.stream_position()? < length {
        let start = reader.stream_position()?;
        let (name, size, header_size) = read_header(&mut reader, length - start)?;
        match &name {
            b"mdat" => media.push(start + header_size..start + size),
            b"moof" => return Err(invalid("暂不支持 fragmented MP4")),
            b"ftyp" | b"moov" => {
                if size > 64 * 1024 * 1024 {
                    return Err(invalid("MP4 元数据超过 64 MiB"));
                }
                let mut payload = vec![0; (size - header_size) as usize];
                reader.read_exact(&mut payload)?;
                let mut data = Vec::new();
                write_box(&mut data, name, &payload)?;
                if name == *b"ftyp" {
                    ftyp = Some(DynBox::demux(&mut io::Cursor::new(Bytes::from(data)))?);
                } else {
                    if moov.is_some() {
                        return Err(invalid("存在多个 moov"));
                    }
                    let mut table_index = 0;
                    let normalized =
                        rewrite_offsets(Bytes::from(data), &mut offsets, &mut table_index, false)?;
                    let mut cursor = io::Cursor::new(Bytes::from(normalized));
                    let (header, payload) = BoxHeader::demux(&mut cursor)?;
                    moov = Some(Moov::demux(header, payload)?);
                }
            }
            _ => {}
        }
        reader.seek(SeekFrom::Start(start + size))?;
    }
    Ok(Input {
        ftyp: ftyp.ok_or_else(|| invalid("ftyp 不存在"))?,
        moov: moov.ok_or_else(|| invalid("moov 不存在"))?,
        media,
        offsets,
    })
}

fn write_box(writer: &mut impl Write, name: [u8; 4], payload: &[u8]) -> io::Result<()> {
    let size = u32::try_from(payload.len() + 8).map_err(|_| invalid("元数据 box 过大"))?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(&name)?;
    writer.write_all(payload)
}

fn rewrite_offsets(
    data: Bytes,
    offsets: &mut Vec<Vec<u64>>,
    table_index: &mut usize,
    output: bool,
) -> io::Result<Vec<u8>> {
    let mut cursor = io::Cursor::new(data);
    let mut result = Vec::new();
    while cursor.position() < cursor.get_ref().len() as u64 {
        let remaining = cursor.get_ref().len() as u64 - cursor.position();
        let (mut name, size, header_size) = read_header(&mut cursor, remaining)?;
        let start = cursor.position() as usize;
        let end = start + (size - header_size) as usize;
        let mut payload = cursor.get_ref().slice(start..end).to_vec();
        cursor.set_position(end as u64);
        match &name {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => {
                payload = rewrite_offsets(Bytes::from(payload), offsets, table_index, output)?;
            }
            b"sbgp" if !output => name = *b"sbgX",
            b"sbgX" if output => name = *b"sbgp",
            b"stco" | b"co64" => {
                if output {
                    let entries = offsets
                        .get(*table_index)
                        .ok_or_else(|| invalid("块偏移轨道数量不一致"))?;
                    name = *b"co64";
                    payload = vec![0; 4];
                    payload.extend_from_slice(
                        &u32::try_from(entries.len())
                            .map_err(|_| invalid("块数量溢出"))?
                            .to_be_bytes(),
                    );
                    for entry in entries {
                        payload.extend_from_slice(&entry.to_be_bytes());
                    }
                } else {
                    let mut reader = io::Cursor::new(&payload);
                    let mut flags = [0; 4];
                    reader.read_exact(&mut flags)?;
                    if flags != [0; 4] {
                        return Err(invalid("块偏移表版本或 flags 不受支持"));
                    }
                    let mut count = [0; 4];
                    reader.read_exact(&mut count)?;
                    let count = u32::from_be_bytes(count) as usize;
                    let width = if name == *b"co64" { 8 } else { 4 };
                    if payload.len() != 8 + count * width {
                        return Err(invalid("块偏移表长度无效"));
                    }
                    let mut entries = Vec::with_capacity(count);
                    for _ in 0..count {
                        let mut offset = [0; 8];
                        reader.read_exact(&mut offset[8 - width..])?;
                        entries.push(u64::from_be_bytes(offset));
                    }
                    offsets.push(entries);
                    name = *b"stco";
                    payload = vec![0; 8 + count * 4];
                    payload[4..8].copy_from_slice(&(count as u32).to_be_bytes());
                }
                *table_index += 1;
            }
            _ => {
                if !output {
                    let mut encoded = Vec::new();
                    write_box(&mut encoded, name, &payload)?;
                    DynBox::demux(&mut io::Cursor::new(Bytes::from(encoded))).map_err(|error| {
                        invalid(&format!("{}: {error}", String::from_utf8_lossy(&name)))
                    })?;
                }
            }
        }
        write_box(&mut result, name, &payload)?;
    }
    Ok(result)
}

fn sample_count(table: &Stbl) -> io::Result<u32> {
    table.stts.entries.iter().try_fold(0u32, |total, entry| {
        total
            .checked_add(entry.sample_count)
            .ok_or_else(|| invalid("样本数量溢出"))
    })
}

fn validate_input(input: &Input) -> io::Result<()> {
    if input.moov.mvex.is_some()
        || input.moov.traks.is_empty()
        || input.media.is_empty()
        || input.moov.mvhd.timescale == 0
    {
        return Err(invalid("需要包含音视频轨道的非分片 MP4"));
    }
    if input.offsets.len() != input.moov.traks.len() {
        return Err(invalid("轨道块偏移表数量不一致"));
    }
    for (index, track) in input.moov.traks.iter().enumerate() {
        let table = &track.mdia.minf.stbl;
        let count = sample_count(table)?;
        if track.mdia.mdhd.timescale == 0 || count == 0 || input.offsets[index].is_empty() {
            return Err(invalid("空轨道或无效时间基"));
        }
        for entry in &table.stsd.entries {
            if !matches!(entry.name(), "hvc1" | "hev1" | "avc1" | "mp4a") {
                return Err(invalid(&format!("不支持的编码标记: {}", entry.name())));
            }
            let mut encoded = Vec::new();
            entry.mux(&mut encoded)?;
            if encoded.get(14..16) != Some(&[0, 1]) {
                return Err(invalid("不支持外部媒体数据引用"));
            }
        }
        if table.stz2.is_some()
            || table.co64.is_some()
            || table.stsh.is_some()
            || table.padb.is_some()
            || table.stdp.is_some()
            || table.sbgp.is_some()
            || table.subs.is_some()
            || table
                .unknown
                .iter()
                .any(|entry| !matches!(entry.name(), "sgpd" | "sbgX"))
        {
            return Err(invalid("存在暂不支持的附加样本表"));
        }
        let sizes = table.stsz.as_ref().ok_or_else(|| invalid("stsz 不存在"))?;
        if sizes.sample_size == 0 && sizes.samples.len() != count as usize {
            return Err(invalid("样本大小表和时间表数量不一致"));
        }
        if let Some(ctts) = &table.ctts
            && ctts
                .entries
                .iter()
                .map(|entry| u64::from(entry.sample_count))
                .sum::<u64>()
                != u64::from(count)
        {
            return Err(invalid("PTS 偏移表样本数量不一致"));
        }
        if let Some(dependencies) = &table.sdtp
            && dependencies.entries.len() != count as usize
        {
            return Err(invalid("样本依赖表数量不一致"));
        }
        if table.stsc.entries.first().map(|entry| entry.first_chunk) != Some(1) {
            return Err(invalid("样本块映射必须从块 1 开始"));
        }
        for group in &table.unknown {
            if let DynBox::Unknown((header, payload)) = group
                && header.box_type == *b"sbgX"
            {
                group_prefix(payload)?;
            }
        }
        let mut sample_index = 0usize;
        for (entry_index, entry) in table.stsc.entries.iter().enumerate() {
            let chunk_end = table
                .stsc
                .entries
                .get(entry_index + 1)
                .map(|next| next.first_chunk as usize - 1)
                .unwrap_or(input.offsets[index].len());
            let chunk_start = entry.first_chunk as usize - 1;
            if chunk_start >= chunk_end
                || chunk_end > input.offsets[index].len()
                || entry.samples_per_chunk == 0
                || entry.sample_description_index == 0
                || entry.sample_description_index as usize > table.stsd.entries.len()
            {
                return Err(invalid("样本块映射或描述索引无效"));
            }
            for &offset in &input.offsets[index][chunk_start..chunk_end] {
                let end = sample_index
                    .checked_add(entry.samples_per_chunk as usize)
                    .ok_or_else(|| invalid("样本索引溢出"))?;
                if end > count as usize {
                    return Err(invalid("样本块数量超过时间表"));
                }
                let chunk_size = if sizes.sample_size == 0 {
                    sizes.samples[sample_index..end]
                        .iter()
                        .map(|size| u64::from(*size))
                        .sum::<u64>()
                } else {
                    u64::from(sizes.sample_size) * u64::from(entry.samples_per_chunk)
                };
                let range = input
                    .media
                    .iter()
                    .find(|range| range.contains(&offset))
                    .ok_or_else(|| invalid("块偏移不在 mdat 内"))?;
                if offset
                    .checked_add(chunk_size)
                    .is_none_or(|end| end > range.end)
                {
                    return Err(invalid("样本块超出 mdat 数据边界"));
                }
                sample_index = end;
            }
        }
        if sample_index != count as usize {
            return Err(invalid("样本块和时间表数量不一致"));
        }
    }
    Ok(())
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn lcm(a: u32, b: u32) -> io::Result<u32> {
    let g = gcd(a, b);
    if g == 0 {
        return Err(invalid("无效的媒体时间基"));
    }
    let value = u64::from(a) / u64::from(g) * u64::from(b);
    u32::try_from(value)
        .map_err(|_| invalid("媒体时间基差异过大，无法无损合并（需重新编码统一时间基）"))
}

// 将轨道时间表缩放到目标时间基（目标为各片段时间基的公倍数，整数倍缩放，无损）。
fn rescale_timing(track: &mut Trak, target_timescale: u32) -> io::Result<()> {
    let source_timescale = track.mdia.mdhd.timescale;
    let factor = u64::from(target_timescale / source_timescale);
    if factor != 1 {
        let table = &mut track.mdia.minf.stbl;
        for entry in &mut table.stts.entries {
            entry.sample_delta = u32::try_from(u64::from(entry.sample_delta) * factor)
                .map_err(|_| invalid("重定时后样本间隔溢出"))?;
        }
        if let Some(ctts) = &mut table.ctts {
            for entry in &mut ctts.entries {
                entry.sample_offset = entry
                    .sample_offset
                    .checked_mul(factor as i64)
                    .ok_or_else(|| invalid("重定时后 PTS 偏移溢出"))?;
            }
        }
    }
    track.mdia.mdhd.timescale = target_timescale;
    Ok(())
}

fn initial_edit_offset(track: &Trak) -> i64 {
    track
        .edts
        .as_ref()
        .and_then(|edits| edits.elst.as_ref())
        .and_then(|elst| elst.entries.iter().find(|entry| entry.media_time >= 0))
        .map(|entry| entry.media_time)
        .unwrap_or(0)
}

fn clear_track(track: &mut Trak) {
    track.tkhd.duration = 0;
    track.tkhd.header.version = 1;
    track.mdia.mdhd.duration = 0;
    track.mdia.mdhd.header.version = 1;
    track.edts = Some(Edts::new(Some(Elst::new(Vec::new()))));
    track
        .edts
        .as_mut()
        .unwrap()
        .elst
        .as_mut()
        .unwrap()
        .header
        .version = 1;
    let table = &mut track.mdia.minf.stbl;
    table.stsd.entries.clear();
    table.stts.entries.clear();
    table.stsc.entries.clear();
    table.stco.entries.clear();
    table.stsz = Some(Stsz::new(0, Vec::new()));
    table.ctts = Some(Ctts {
        header: FullBoxHeader::new(*b"ctts", 1, 0),
        entries: Vec::new(),
    });
    table.stss = Some(Stss {
        header: FullBoxHeader::new(*b"stss", 0, 0),
        entries: Vec::new(),
    });
    if let Some(dependencies) = &mut table.sdtp {
        dependencies.entries.clear();
    }
    for entry in &mut table.unknown {
        if let DynBox::Unknown((header, payload)) = entry
            && header.box_type == *b"sbgX"
        {
            let prefix = if payload[0] == 1 { 12 } else { 8 };
            let mut empty = payload[..prefix].to_vec();
            empty.extend_from_slice(&0u32.to_be_bytes());
            *payload = Bytes::from(empty);
        }
    }
}

fn append_track(
    target: &mut Trak,
    source: &Trak,
    segment_duration: u64,
    offsets: &mut Vec<u64>,
    source_offsets: &[u64],
) -> io::Result<()> {
    if target.mdia.hdlr.handler_type != source.mdia.hdlr.handler_type {
        return Err(invalid(&format!(
            "轨道类型不一致: {:?} vs {:?}",
            target.mdia.hdlr.handler_type, source.mdia.hdlr.handler_type
        )));
    }
    if target.mdia.mdhd.timescale != source.mdia.mdhd.timescale {
        return Err(invalid(&format!(
            "时间基不一致: {} vs {}",
            target.mdia.mdhd.timescale, source.mdia.mdhd.timescale
        )));
    }
    if target.tkhd.width != source.tkhd.width || target.tkhd.height != source.tkhd.height {
        return Err(invalid(&format!(
            "分辨率不一致: {}x{} vs {}x{}",
            target.tkhd.width.to_num::<u32>(),
            target.tkhd.height.to_num::<u32>(),
            source.tkhd.width.to_num::<u32>(),
            source.tkhd.height.to_num::<u32>()
        )));
    }
    let media_start = target.mdia.mdhd.duration;
    let source_table = &source.mdia.minf.stbl;
    let target_table = &mut target.mdia.minf.stbl;
    if let Some(existing) = target_table.stsd.entries.first() {
        let family = |name: &str| match name {
            "hvc1" | "hev1" => 1,
            "avc1" => 2,
            "mp4a" => 3,
            _ => 0,
        };
        if source_table
            .stsd
            .entries
            .iter()
            .any(|entry| family(entry.name()) != family(existing.name()))
        {
            return Err(invalid("片段编码格式不一致"));
        }
        if let DynBox::Mp4a(existing) = existing
            && source_table.stsd.entries.iter().any(|entry| {
                !matches!(entry, DynBox::Mp4a(source) if source.audio_sample_entry == existing.audio_sample_entry)
            })
        {
            return Err(invalid("音频采样率或声道配置不一致"));
        }
    }
    let sample_start = sample_count(target_table)?;
    let count = sample_count(source_table)?;
    append_groups(target_table, source_table, count)?;
    sample_start
        .checked_add(count)
        .ok_or_else(|| invalid("样本总数量溢出"))?;
    let chunk_start = u32::try_from(offsets.len()).map_err(|_| invalid("块数量溢出"))?;
    let mut descriptions = Vec::new();
    for entry in &source_table.stsd.entries {
        let index = match target_table
            .stsd
            .entries
            .iter()
            .position(|existing| existing == entry)
        {
            Some(index) => index,
            None => {
                target_table.stsd.entries.push(entry.clone());
                target_table.stsd.entries.len() - 1
            }
        };
        descriptions.push(u32::try_from(index + 1).map_err(|_| invalid("样本描述数量溢出"))?);
    }
    for entry in &source_table.stsc.entries {
        let mut entry = entry.clone();
        entry.first_chunk = entry
            .first_chunk
            .checked_add(chunk_start)
            .ok_or_else(|| invalid("块索引溢出"))?;
        entry.sample_description_index = *descriptions
            .get(entry.sample_description_index.wrapping_sub(1) as usize)
            .ok_or_else(|| invalid("无效样本描述索引"))?;
        target_table.stsc.entries.push(entry);
    }
    offsets.extend_from_slice(source_offsets);
    target_table
        .stts
        .entries
        .extend_from_slice(&source_table.stts.entries);
    let sizes = source_table.stsz.as_ref().unwrap();
    let target_sizes = &mut target_table.stsz.as_mut().unwrap().samples;
    if sizes.sample_size == 0 {
        target_sizes.extend_from_slice(&sizes.samples);
    } else {
        target_sizes.extend(std::iter::repeat_n(sizes.sample_size, count as usize));
    }
    let composition = &mut target_table.ctts.as_mut().unwrap().entries;
    match &source_table.ctts {
        Some(ctts) => composition.extend_from_slice(&ctts.entries),
        None => composition.push(CttsEntry {
            sample_count: count,
            sample_offset: 0,
        }),
    }
    let sync = &mut target_table.stss.as_mut().unwrap().entries;
    match &source_table.stss {
        Some(stss) => {
            for &sample in &stss.entries {
                if sample == 0 || sample > count {
                    return Err(invalid("关键帧索引无效"));
                }
                sync.push(sample_start + sample);
            }
        }
        None => sync.extend(sample_start + 1..=sample_start + count),
    }
    match (&mut target_table.sdtp, &source_table.sdtp) {
        (Some(target), Some(source)) => target.entries.extend_from_slice(&source.entries),
        (None, None) => {}
        _ => return Err(invalid("样本依赖表配置不一致")),
    }
    let duration = source_table
        .stts
        .entries
        .iter()
        .try_fold(0u64, |total, entry| {
            total
                .checked_add(u64::from(entry.sample_count) * u64::from(entry.sample_delta))
                .ok_or_else(|| invalid("媒体时长溢出"))
        })?;
    target.mdia.mdhd.duration = media_start
        .checked_add(duration)
        .ok_or_else(|| invalid("媒体总时长溢出"))?;
    target.tkhd.duration = target
        .tkhd
        .duration
        .checked_add(segment_duration)
        .ok_or_else(|| invalid("轨道总时长溢出"))?;
    Ok(())
}

fn group_prefix(payload: &[u8]) -> io::Result<usize> {
    if payload.len() < 12 || payload[0] > 1 || payload[1..4] != [0; 3] {
        return Err(invalid("样本分组表 header 无效"));
    }
    let prefix = if payload[0] == 1 { 12 } else { 8 };
    if payload.len() < prefix + 4 {
        return Err(invalid("样本分组表不完整"));
    }
    let count = u32::from_be_bytes(payload[prefix..prefix + 4].try_into().unwrap()) as usize;
    if payload.len() != prefix + 4 + count * 8 {
        return Err(invalid("样本分组表长度无效"));
    }
    Ok(prefix)
}

fn append_groups(target: &mut Stbl, source: &Stbl, sample_count: u32) -> io::Result<()> {
    let descriptions = |table: &Stbl| {
        table
            .unknown
            .iter()
            .filter(|entry| entry.name() == "sgpd")
            .cloned()
            .collect::<Vec<_>>()
    };
    if descriptions(target) != descriptions(source) {
        return Err(invalid("样本分组描述不一致"));
    }
    let source_groups = source
        .unknown
        .iter()
        .filter(|entry| entry.name() == "sbgX")
        .collect::<Vec<_>>();
    if source_groups.len()
        != target
            .unknown
            .iter()
            .filter(|entry| entry.name() == "sbgX")
            .count()
    {
        return Err(invalid("样本分组数量不一致"));
    }
    for source_entry in source_groups {
        let DynBox::Unknown((_, source_payload)) = source_entry else {
            unreachable!()
        };
        let prefix = group_prefix(source_payload)?;
        let target_entry = target.unknown.iter_mut().find(|entry| {
            matches!(entry, DynBox::Unknown((header, payload)) if header.box_type == *b"sbgX" && payload.get(..prefix) == source_payload.get(..prefix))
        }).ok_or_else(|| invalid("样本分组类型不一致"))?;
        let DynBox::Unknown((_, target_payload)) = target_entry else {
            unreachable!()
        };
        let target_prefix = group_prefix(target_payload)?;
        if target_prefix != prefix {
            return Err(invalid("样本分组版本不一致"));
        }
        let mut combined = target_payload.to_vec();
        combined.extend_from_slice(&source_payload[prefix + 4..]);
        let grouped_count = source_payload[prefix + 4..]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|entry| u64::from(u32::from_be_bytes(entry[..4].try_into().unwrap())))
            .sum::<u64>();
        if grouped_count > u64::from(sample_count) {
            return Err(invalid("样本分组数量超过轨道样本数"));
        }
        if grouped_count < u64::from(sample_count) {
            combined.extend_from_slice(&(sample_count - grouped_count as u32).to_be_bytes());
            combined.extend_from_slice(&0u32.to_be_bytes());
        }
        let count = u32::try_from((combined.len() - prefix - 4) / 8)
            .map_err(|_| invalid("样本分组数量溢出"))?;
        combined[prefix..prefix + 4].copy_from_slice(&count.to_be_bytes());
        *target_payload = Bytes::from(combined);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use scuffle_mp4::types::{
        hdlr::{HandlerType, Hdlr},
        mdhd::Mdhd,
        mdia::Mdia,
        minf::Minf,
        mvhd::Mvhd,
        stco::Stco,
        stsc::{Stsc, StscEntry},
        stsd::Stsd,
        stts::{Stts, SttsEntry},
        tkhd::Tkhd,
    };

    fn fixture(path: &Path) -> io::Result<()> {
        let ftyp = DynBox::Unknown((
            BoxHeader::new(*b"ftyp"),
            Bytes::from_static(b"isom\0\0\0\0isom"),
        ));
        let mut payload = vec![0; 78];
        payload[7] = 1;
        let table = Stbl::new(
            Stsd::new(vec![DynBox::Unknown((
                BoxHeader::new(*b"hvc1"),
                Bytes::from(payload),
            ))]),
            Stts::new(vec![SttsEntry {
                sample_count: 2,
                sample_delta: 10,
            }]),
            Stsc::new(vec![StscEntry {
                first_chunk: 1,
                samples_per_chunk: 2,
                sample_description_index: 1,
            }]),
            Stco {
                header: FullBoxHeader::new(*b"stco", 0, 0),
                entries: vec![(ftyp.size() + 8) as u32],
            },
            Some(Stsz::new(0, vec![2, 3])),
        );
        let track = Trak::new(
            Tkhd::new(0, 0, 1, 20, Some((32, 32))),
            None,
            Mdia::new(
                Mdhd::new(0, 0, 1000, 20),
                Hdlr::new(HandlerType::Vide, "Video".into()),
                Minf::new(table, None, None),
            ),
        );
        let movie = Moov::new(Mvhd::new(0, 0, 1000, 20, 2), vec![track], None);
        let mut writer = File::create(path)?;
        ftyp.mux(&mut writer)?;
        write_box(&mut writer, *b"mdat", &[1, 2, 3, 4, 5])?;
        movie.mux(&mut writer)
    }

    #[test]
    fn single_edit_list_preserves_initial_offset() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("1.mp4");
        let second = directory.path().join("2.mp4");
        let output = directory.path().join("merged.mp4");
        for path in [&first, &second] {
            fixture(path)?;
            let mut input = read_input(path)?;
            input.moov.traks[0].mdia.minf.stbl.stco.entries = vec![(input.ftyp.size() + 8) as u32];
            input.moov.traks[0].edts = Some(Edts::new(Some(Elst::new(vec![ElstEntry {
                segment_duration: 20,
                media_time: 5,
                media_rate_integer: 1,
                media_rate_fraction: 0,
            }]))));
            let mut writer = File::create(path)?;
            input.ftyp.mux(&mut writer)?;
            write_box(&mut writer, *b"mdat", &[1, 2, 3, 4, 5])?;
            input.moov.mux(&mut writer)?;
        }
        merge(&[first, second], &output, false)?;
        let merged = read_input(&output)?;
        let edits = &merged.moov.traks[0]
            .edts
            .as_ref()
            .unwrap()
            .elst
            .as_ref()
            .unwrap()
            .entries;
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].media_time, 5);
        assert_eq!(edits[0].segment_duration, 40);
        Ok(())
    }

    #[test]
    fn different_timescales_are_rescaled() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("1.mp4");
        let second = directory.path().join("2.mp4");
        let output = directory.path().join("merged.mp4");
        fixture(&first)?;
        fixture(&second)?;
        let mut input = read_input(&second)?;
        input.moov.traks[0].mdia.mdhd.timescale = 2000;
        input.moov.traks[0].mdia.minf.stbl.stts = Stts::new(vec![SttsEntry {
            sample_count: 2,
            sample_delta: 20,
        }]);
        input.moov.traks[0].mdia.minf.stbl.stco.entries = vec![(input.ftyp.size() + 8) as u32];
        let mut writer = File::create(&second)?;
        input.ftyp.mux(&mut writer)?;
        write_box(&mut writer, *b"mdat", &[1, 2, 3, 4, 5])?;
        input.moov.mux(&mut writer)?;
        drop(writer);

        merge(&[first, second], &output, false)?;
        let merged = read_input(&output)?;
        let track = &merged.moov.traks[0];
        assert_eq!(track.mdia.mdhd.timescale, 2000);
        assert!(
            track
                .mdia
                .minf
                .stbl
                .stts
                .entries
                .iter()
                .all(|entry| entry.sample_delta == 20)
        );
        assert_eq!(sample_count(&track.mdia.minf.stbl)?, 4);
        let mut reader = File::open(output)?;
        reader.seek(SeekFrom::Start(merged.media[0].start))?;
        let mut payload = vec![0; 10];
        reader.read_exact(&mut payload)?;
        assert_eq!(payload, vec![1, 2, 3, 4, 5, 1, 2, 3, 4, 5]);
        Ok(())
    }

    #[test]
    fn offsets_round_trip_above_four_gib() -> io::Result<()> {
        let expected = vec![vec![32, u64::from(u32::MAX) + 100]];
        let mut placeholder = Vec::new();
        write_box(&mut placeholder, *b"stco", &[0; 8])?;
        let encoded = rewrite_offsets(
            Bytes::from(placeholder),
            &mut expected.clone(),
            &mut 0,
            true,
        )?;
        assert_eq!(&encoded[4..8], b"co64");
        let mut actual = Vec::new();
        rewrite_offsets(Bytes::from(encoded), &mut actual, &mut 0, false)?;
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn merge_preserves_media_and_accumulates_tables() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("1.mp4");
        let second = directory.path().join("2.mp4");
        let output = directory.path().join("merged.mp4");
        fixture(&first)?;
        fixture(&second)?;
        merge(&[first, second], &output, false)?;
        let input = read_input(&output)?;
        validate_input(&input)?;
        let track = &input.moov.traks[0];
        assert_eq!(input.moov.mvhd.duration, 40);
        assert_eq!(track.mdia.mdhd.duration, 40);
        assert_eq!(sample_count(&track.mdia.minf.stbl)?, 4);
        assert_eq!(track.mdia.minf.stbl.stsd.entries.len(), 1);
        let edits = &track.edts.as_ref().unwrap().elst.as_ref().unwrap().entries;
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].media_time, 0);
        assert_eq!(edits[0].segment_duration, 40);
        let mut reader = File::open(output)?;
        reader.seek(SeekFrom::Start(input.media[0].start))?;
        let mut payload = vec![0; 10];
        reader.read_exact(&mut payload)?;
        assert_eq!(payload, vec![1, 2, 3, 4, 5, 1, 2, 3, 4, 5]);
        Ok(())
    }

    #[test]
    fn constant_sample_sizes_are_expanded() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("input.mp4");
        fixture(&path)?;
        let mut input = read_input(&path)?;
        let source = &mut input.moov.traks[0];
        source.mdia.minf.stbl.stsz = Some(Stsz::new(4, Vec::new()));
        let mut target = source.clone();
        clear_track(&mut target);
        append_track(&mut target, source, 20, &mut Vec::new(), &[32])?;
        assert_eq!(
            target.mdia.minf.stbl.stsz.as_ref().unwrap().samples,
            vec![4, 4]
        );
        Ok(())
    }

    #[test]
    fn sample_groups_preserve_ungrouped_tail() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("input.mp4");
        fixture(&path)?;
        let mut input = read_input(&path)?;
        let source = &mut input.moov.traks[0];
        let mut payload = vec![0; 4];
        payload.extend_from_slice(b"roll");
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        source.mdia.minf.stbl.unknown.push(DynBox::Unknown((
            BoxHeader::new(*b"sbgX"),
            Bytes::from(payload),
        )));
        let mut target = source.clone();
        clear_track(&mut target);
        append_track(&mut target, source, 20, &mut Vec::new(), &[32])?;
        append_track(&mut target, source, 20, &mut vec![32], &[40])?;
        let DynBox::Unknown((_, payload)) = &target.mdia.minf.stbl.unknown[0] else {
            unreachable!()
        };
        assert_eq!(group_prefix(payload)?, 8);
        assert_eq!(u32::from_be_bytes(payload[8..12].try_into().unwrap()), 4);
        assert_eq!(&payload[20..28], &[0, 0, 0, 1, 0, 0, 0, 0]);
        Ok(())
    }

    #[test]
    fn invalid_input_does_not_publish_output() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("1.mp4");
        let broken = directory.path().join("2.mp4");
        let output = directory.path().join("merged.mp4");
        fixture(&first)?;
        std::fs::write(&broken, b"broken")?;
        assert!(merge(&[first, broken], &output, false).is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 2);
        Ok(())
    }

    #[test]
    fn existing_output_is_not_overwritten() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let input = directory.path().join("input.mp4");
        let output = directory.path().join("merged.mp4");
        fixture(&input)?;
        std::fs::write(&output, b"existing")?;
        assert!(merge(&[input], &output, false).is_err());
        assert_eq!(std::fs::read(output)?, b"existing");
        Ok(())
    }

    #[test]
    fn existing_output_is_overwritten_when_allowed() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let input = directory.path().join("input.mp4");
        let output = directory.path().join("merged.mp4");
        fixture(&input)?;
        std::fs::write(&output, b"existing")?;
        merge(std::slice::from_ref(&input), &output, true)?;
        let merged = read_input(&output)?;
        validate_input(&merged)?;
        assert_ne!(std::fs::read(output)?, b"existing");
        Ok(())
    }

    #[test]
    fn incompatible_track_cleans_up_temporary_output() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("1.mp4");
        let second = directory.path().join("2.mp4");
        let output = directory.path().join("merged.mp4");
        fixture(&first)?;
        fixture(&second)?;
        let mut input = read_input(&second)?;
        if let DynBox::Unknown((header, _)) =
            &mut input.moov.traks[0].mdia.minf.stbl.stsd.entries[0]
        {
            header.box_type = *b"avc1";
        }
        input.moov.traks[0].mdia.minf.stbl.stco.entries = vec![(input.ftyp.size() + 8) as u32];
        let mut writer = File::create(&second)?;
        input.ftyp.mux(&mut writer)?;
        write_box(&mut writer, *b"mdat", &[1, 2, 3, 4, 5])?;
        input.moov.mux(&mut writer)?;
        drop(writer);
        assert!(merge(&[first, second], &output, false).is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 2);
        Ok(())
    }
}
