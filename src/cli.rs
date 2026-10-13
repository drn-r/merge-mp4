use std::path::PathBuf;

use clap::Parser;

fn parse_positive_core(value: &str) -> Result<usize, String> {
    let cores = value
        .parse::<usize>()
        .map_err(|_| "--core 必须是大于 0 的整数".to_owned())?;
    if cores == 0 {
        return Err("--core 必须是大于 0 的整数".to_owned());
    }
    Ok(cores)
}

/// 扫描目录中的 mp4 文件，按自然顺序排序后拼接为单个视频文件。
#[derive(Parser)]
#[command(name = "merge-mp4", version, about)]
pub struct Cli {
    /// 包含 mp4 文件的目录（必填）
    pub directory: PathBuf,

    /// 输出文件路径（默认在源目录的 output 子目录生成 merged.mp4）
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
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn core_count_requires_convert_and_must_be_positive() {
        assert!(Cli::try_parse_from(["merge-mp4", "videos", "--core", "8"]).is_err());
        assert!(Cli::try_parse_from(["merge-mp4", "videos", "--convert", "--core", "0"]).is_err());
        let cli = Cli::try_parse_from(["merge-mp4", "videos", "--convert", "--core", "8"]).unwrap();
        assert_eq!(cli.core, Some(8));
    }
}
