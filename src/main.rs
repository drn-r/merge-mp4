mod cli;
mod merger;
mod natural_sort;
mod scanner;

use std::error::Error;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Parser;

use cli::Cli;
use merger::merge;
use scanner::{file_name, scan_mp4_files};

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    if !cli.directory.is_dir() {
        return Err(format!("目录不存在或不是目录: {}", cli.directory.display()).into());
    }

    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| cli.directory.join("merged.mp4"));

    let files = scan_mp4_files(&cli.directory, &output)?;
    if files.is_empty() {
        return Err("目录中没有找到 mp4 文件".into());
    }

    print_preview(&files, &output);

    if cli.sort_only {
        return Ok(());
    }

    if !confirm("\n开始合并？ [Y/n] ")? {
        println!("已取消。");
        return Ok(());
    }

    merge(&files, &output)?;
    println!("\n完成！已输出: {}", output.display());
    Ok(())
}

fn print_preview(files: &[PathBuf], output: &Path) {
    println!("找到 {} 个 MP4 文件：\n", files.len());
    let width = files.len().to_string().len();
    for (i, f) in files.iter().enumerate() {
        println!("[{:0width$}] {}", i + 1, file_name(f), width = width);
    }
    println!("\n输出：\n{}", output.display());
}

fn confirm(prompt: &str) -> io::Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}
