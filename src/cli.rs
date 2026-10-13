use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// 转码后的目标分辨率（按高度缩放并保持宽高比）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Resolution {
    /// 保持源分辨率（默认）
    Source,
    /// 2160p（4K）
    #[value(name = "2160p", alias = "4k")]
    P2160,
    /// 1080p
    #[value(name = "1080p")]
    P1080,
    /// 720p
    #[value(name = "720p")]
    P720,
    /// 480p
    #[value(name = "480p")]
    P480,
    /// 360p
    #[value(name = "360p")]
    P360,
}

impl Resolution {
    /// 返回目标高度；`Source` 表示沿用源分辨率。
    pub fn target_height(self) -> Option<u32> {
        match self {
            Resolution::Source => None,
            Resolution::P2160 => Some(2160),
            Resolution::P1080 => Some(1080),
            Resolution::P720 => Some(720),
            Resolution::P480 => Some(480),
            Resolution::P360 => Some(360),
        }
    }

    /// 用于输出文件名的分辨率标签；`Source` 不追加标签。
    pub fn label(self) -> Option<&'static str> {
        match self {
            Resolution::Source => None,
            Resolution::P2160 => Some("2160p"),
            Resolution::P1080 => Some("1080p"),
            Resolution::P720 => Some("720p"),
            Resolution::P480 => Some("480p"),
            Resolution::P360 => Some("360p"),
        }
    }
}

fn parse_positive_core(value: &str) -> Result<usize, String> {
    let cores = value
        .parse::<usize>()
        .map_err(|_| "--core 必须是大于 0 的整数".to_owned())?;
    if cores == 0 {
        return Err("--core 必须是大于 0 的整数".to_owned());
    }
    Ok(cores)
}

fn parse_positive_split(value: &str) -> Result<usize, String> {
    let parts = value
        .parse::<usize>()
        .map_err(|_| "--split 必须是大于 0 的整数".to_owned())?;
    if parts == 0 {
        return Err("--split 必须是大于 0 的整数".to_owned());
    }
    Ok(parts)
}

/// 扫描目录中的 mp4 文件，按自然顺序排序后拼接为单个视频文件。
#[derive(Parser)]
#[command(name = "merge-mp4", version, about)]
pub struct Cli {
    /// 包含 mp4 文件的目录（必填）
    pub directory: PathBuf,

    /// 输出文件路径（默认在源目录的 output 子目录生成 以目录名命名的 mp4）
    #[arg(short = 'o', long = "output")]
    pub output: Option<PathBuf>,

    /// 仅输出排序后的预览，不执行合并
    #[arg(short = 's', long = "sort-only")]
    pub sort_only: bool,

    /// 使用 FFmpeg concat demuxer 合并（需要安装 ffmpeg）
    #[arg(long)]
    pub ffmpeg: bool,

    /// 先并行转码为 H.264/AAC，再使用 FFmpeg 合并
    #[arg(long, conflicts_with = "ffmpeg")]
    pub convert: bool,

    /// 转码时并行运行的 FFmpeg 进程数（默认最多 4）
    #[arg(long, requires = "convert", value_name = "N", value_parser = parse_positive_core)]
    pub core: Option<usize>,

    /// 转码后的目标分辨率（按高度缩放并保持宽高比；默认保持源分辨率）
    #[arg(
        long,
        requires = "convert",
        value_name = "RES",
        default_value = "source"
    )]
    pub resolution: Resolution,

    /// 将文件平均分成若干组，分别合并输出（文件名追加 _1、_2 ……）
    #[arg(long, value_name = "N", value_parser = parse_positive_split)]
    pub split: Option<usize>,

    /// 静默模式：跳过所有确认，已存在的输出文件直接覆盖
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Resolution};
    use clap::Parser;

    #[test]
    fn core_count_requires_convert_and_must_be_positive() {
        assert!(Cli::try_parse_from(["merge-mp4", "videos", "--core", "8"]).is_err());
        assert!(Cli::try_parse_from(["merge-mp4", "videos", "--convert", "--core", "0"]).is_err());
        let cli = Cli::try_parse_from(["merge-mp4", "videos", "--convert", "--core", "8"]).unwrap();
        assert_eq!(cli.core, Some(8));
    }

    #[test]
    fn resolution_requires_convert_and_defaults_to_source() {
        assert!(Cli::try_parse_from(["merge-mp4", "videos", "--resolution", "720p"]).is_err());
        let cli = Cli::try_parse_from(["merge-mp4", "videos", "--convert"]).unwrap();
        assert_eq!(cli.resolution, Resolution::Source);
        let cli =
            Cli::try_parse_from(["merge-mp4", "videos", "--convert", "--resolution", "1080p"])
                .unwrap();
        assert_eq!(cli.resolution, Resolution::P1080);
        assert_eq!(cli.resolution.target_height(), Some(1080));
    }

    #[test]
    fn split_must_be_positive() {
        assert!(Cli::try_parse_from(["merge-mp4", "videos", "--split", "0"]).is_err());
        let cli = Cli::try_parse_from(["merge-mp4", "videos", "--split", "4"]).unwrap();
        assert_eq!(cli.split, Some(4));
    }
}
