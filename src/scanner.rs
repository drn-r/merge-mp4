use std::io;
use std::path::{Path, PathBuf};

use crate::natural_sort::natural_cmp;

/// 扫描目录中的所有 mp4 文件，排除输出文件自身，并按自然顺序排序。
pub fn scan_mp4_files(dir: &Path, output: &Path) -> io::Result<Vec<PathBuf>> {
    let output_abs = output.canonicalize().ok();
    let mut files = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
        {
            continue;
        }
        // 排除输出文件自身（避免把上一次的合并结果也包含进来）
        if let Some(ref out_abs) = output_abs {
            if path.canonicalize().ok().as_ref() == Some(out_abs) {
                continue;
            }
        }
        files.push(path);
    }

    files.sort_by(|a, b| natural_cmp(file_name(a), file_name(b)));
    Ok(files)
}

pub fn file_name(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}
