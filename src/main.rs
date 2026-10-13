mod cli;
mod merger;
mod natural_sort;
mod scanner;

use std::error::Error;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::Parser;

use cli::Cli;
use merger::{merge, merge_with_conversion, merge_with_ffmpeg};
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

    let files = scan_mp4_files(&cli.directory, &output_path(&cli, 1, 1))?;
    if files.is_empty() {
        return Err("目录中没有找到 mp4 文件".into());
    }

    let part_count = cli.split.unwrap_or(1).min(files.len());
    let chunks = split_files(&files, part_count);
    let outputs: Vec<PathBuf> = (0..part_count)
        .map(|index| output_path(&cli, index + 1, part_count))
        .collect();

    print_preview(&chunks, &outputs);

    if cli.sort_only {
        return Ok(());
    }

    if !cli.yes && !confirm("\n开始合并？ [Y/n] ")? {
        println!("已取消。");
        return Ok(());
    }

    let mut overwrite = cli.yes;
    let existing: Vec<&PathBuf> = outputs.iter().filter(|path| path.exists()).collect();
    if !existing.is_empty() && !cli.yes {
        let list = existing
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if !confirm(&format!("\n以下输出文件已存在:\n{list}\n覆盖？ [Y/n] "))? {
            println!("已取消。");
            return Ok(());
        }
        overwrite = true;
    }

    for output in &outputs {
        if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
    }

    for (index, output) in outputs.iter().enumerate() {
        let chunk = chunks[index];
        if part_count > 1 {
            println!(
                "\n=== 第 {}/{} 组（{} 个文件）===",
                index + 1,
                part_count,
                chunk.len()
            );
        }
        if cli.convert {
            merge_with_conversion(
                chunk,
                output,
                overwrite,
                cli.core,
                cli.resolution.target_height(),
            )?;
        } else if cli.ffmpeg {
            merge_with_ffmpeg(chunk, output, overwrite)?;
        } else {
            merge(chunk, output, overwrite)?;
        }
        println!("已输出: {}", output.display());
    }
    println!("\n完成！");
    Ok(())
}

/// 将文件按顺序平均分成 `parts` 组，前若干组多分 1 个以吸收余数。
fn split_files(files: &[PathBuf], parts: usize) -> Vec<&[PathBuf]> {
    let base = files.len() / parts;
    let remainder = files.len() % parts;
    let mut chunks = Vec::with_capacity(parts);
    let mut start = 0;
    for index in 0..parts {
        let len = base + usize::from(index < remainder);
        chunks.push(&files[start..start + len]);
        start += len;
    }
    chunks
}

fn output_path(cli: &Cli, index: usize, total: usize) -> PathBuf {
    if let Some(base) = &cli.output {
        if total > 1 {
            let stem = base
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "merged".to_owned());
            let ext = base
                .extension()
                .map(|ext| ext.to_string_lossy().into_owned())
                .unwrap_or_else(|| "mp4".to_owned());
            base.with_file_name(format!("{stem}_{index}.{ext}"))
        } else {
            base.clone()
        }
    } else {
        cli.directory
            .join("output")
            .join(output_file_name(cli, index, total))
    }
}

fn output_file_name(cli: &Cli, index: usize, total: usize) -> String {
    let mut name = cli
        .directory
        .canonicalize()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "merged".to_owned());
    if total > 1 {
        name.push('_');
        name.push_str(&index.to_string());
    }
    if cli.convert {
        name.push_str("_converted");
        if let Some(label) = cli.resolution.label() {
            name.push('_');
            name.push_str(label);
        }
    }
    name.push_str(".mp4");
    name
}

fn print_preview(chunks: &[&[PathBuf]], outputs: &[PathBuf]) {
    let total: usize = chunks.iter().map(|chunk| chunk.len()).sum();
    println!("找到 {total} 个 MP4 文件：\n");
    let width = total.to_string().len();
    let mut counter = 0;
    for (group, chunk) in chunks.iter().enumerate() {
        if chunks.len() > 1 {
            println!(
                "— 第 {}/{} 组（{} 个文件）—",
                group + 1,
                chunks.len(),
                chunk.len()
            );
        }
        for file in chunk.iter() {
            counter += 1;
            println!("[{:0width$}] {}", counter, file_name(file), width = width);
        }
    }
    println!("\n输出：");
    for output in outputs {
        println!("{}", output.display());
    }
}

fn confirm(prompt: &str) -> io::Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}
