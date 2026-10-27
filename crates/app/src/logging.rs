//! 本地日志：每次启动一个 `logs/yyyy-MM-dd HH-mm-ss.log`，只记录启动耗时和错误。

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

const KEEP: usize = 20;

pub struct Log {
    file: Option<File>,
}

/// 日志目录：环境变量 `TML_LOG_DIR`，否则为可执行文件旁的 `logs`。
fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("TML_LOG_DIR") {
        return Some(d.into());
    }
    Some(std::env::current_exe().ok()?.parent()?.join("logs"))
}

impl Log {
    pub fn open() -> Self {
        let file = dir().and_then(|d| {
            fs::create_dir_all(&d).ok()?;
            prune(&d);
            File::create(d.join(format!("{}.log", tml_core::local_timestamp()))).ok()
        });
        Self { file }
    }

    pub fn line(&mut self, msg: impl AsRef<str>) {
        if let Some(f) = &mut self.file {
            let _ = writeln!(f, "{} {}", tml_core::local_timestamp(), msg.as_ref());
        }
    }
}

/// 只保留最近的若干个日志文件。文件名即时间戳，按名称排序即按时间排序。
fn prune(dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> =
        entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "log")).collect();
    if logs.len() < KEEP {
        return;
    }
    logs.sort();
    for p in &logs[..logs.len() + 1 - KEEP] {
        let _ = fs::remove_file(p);
    }
}
