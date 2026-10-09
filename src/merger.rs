use std::error::Error;
use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use mp4::{
    AacConfig, AvcConfig, HevcConfig, MediaConfig, MediaType, Mp4Config, Mp4Reader, Mp4Track,
    Mp4Writer, TrackConfig, TtxtConfig, Vp9Config,
};

use crate::scanner::file_name;

/// 使用 mp4 crate 将多个文件无损拼接为单个 mp4 输出。
pub fn merge(files: &[PathBuf], output: &Path) -> Result<(), Box<dyn Error>> {
    // 以第一个文件作为模板建立输出容器与轨道。
    let first = open_reader(&files[0])?;

    let config = Mp4Config {
        major_brand: *first.major_brand(),
        minor_version: first.minor_version(),
        compatible_brands: first.compatible_brands().to_vec(),
        timescale: first.timescale(),
    };

    let dst = File::create(output)?;
    let mut writer = Mp4Writer::write_start(BufWriter::new(dst), &config)?;

    let base_track_ids = sorted_track_ids(&first);
    for tid in &base_track_ids {
        let track = &first.tracks()[tid];
        writer.add_track(&track_config_from(track)?)?;
    }
    // Mp4Writer 按 add_track 的顺序分配轨道 id：1, 2, 3, ...
    let writer_track_ids: Vec<u32> = (1..=base_track_ids.len() as u32).collect();
    let base_timescales: Vec<u32> = base_track_ids
        .iter()
        .map(|t| first.tracks()[t].timescale())
        .collect();
    drop(first);

    for (fi, path) in files.iter().enumerate() {
        println!("[{}/{}] 合并 {}", fi + 1, files.len(), file_name(path));
        let mut reader = open_reader(path)?;
        let track_ids = sorted_track_ids(&reader);

        if track_ids.len() != base_track_ids.len() {
            return Err(format!(
                "{} 的轨道数量({})与第一个文件({})不一致，无法无损拼接",
                file_name(path),
                track_ids.len(),
                base_track_ids.len()
            )
            .into());
        }

        for (idx, tid) in track_ids.iter().enumerate() {
            let timescale = reader.tracks()[tid].timescale();
            if timescale != base_timescales[idx] {
                eprintln!(
                    "警告: {} 轨道 {} 的 timescale({})与模板({})不同，拼接后时间轴可能不正确",
                    file_name(path),
                    tid,
                    timescale,
                    base_timescales[idx]
                );
            }

            let writer_tid = writer_track_ids[idx];
            let count = reader.sample_count(*tid)?;
            for sample_idx in 0..count {
                if let Some(sample) = reader.read_sample(*tid, sample_idx + 1)? {
                    writer.write_sample(writer_tid, &sample)?;
                }
            }
        }
    }

    writer.write_end()?;
    Ok(())
}

fn open_reader(path: &Path) -> Result<Mp4Reader<BufReader<File>>, Box<dyn Error>> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    Ok(Mp4Reader::read_header(BufReader::new(file), size)?)
}

fn sorted_track_ids<R: io::Read + io::Seek>(reader: &Mp4Reader<R>) -> Vec<u32> {
    let mut ids: Vec<u32> = reader.tracks().keys().copied().collect();
    ids.sort_unstable();
    ids
}

fn track_config_from(track: &Mp4Track) -> Result<TrackConfig, Box<dyn Error>> {
    let media_conf = match track.media_type()? {
        MediaType::H264 => MediaConfig::AvcConfig(AvcConfig {
            width: track.width(),
            height: track.height(),
            seq_param_set: track.sequence_parameter_set()?.to_vec(),
            pic_param_set: track.picture_parameter_set()?.to_vec(),
        }),
        MediaType::H265 => MediaConfig::HevcConfig(HevcConfig {
            width: track.width(),
            height: track.height(),
        }),
        MediaType::VP9 => MediaConfig::Vp9Config(Vp9Config {
            width: track.width(),
            height: track.height(),
        }),
        MediaType::AAC => MediaConfig::AacConfig(AacConfig {
            bitrate: track.bitrate(),
            profile: track.audio_profile()?,
            freq_index: track.sample_freq_index()?,
            chan_conf: track.channel_config()?,
        }),
        MediaType::TTXT => MediaConfig::TtxtConfig(TtxtConfig {}),
    };

    Ok(TrackConfig {
        track_type: track.track_type()?,
        timescale: track.timescale(),
        language: track.language().to_string(),
        media_conf,
    })
}
