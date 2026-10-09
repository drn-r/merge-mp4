use std::path::PathBuf;

use clap::Parser;

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
}
