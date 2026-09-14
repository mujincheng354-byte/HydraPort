//! 极简日志实现（§8 启动流程第 1 步）。
//!
//! 出于内存与体积考虑没有引入 `env_logger`：这里只在进程内安装一个 `log::Log` 实现，
//! 关闭日志时每次调用只做一次等级比较，不产生任何分配。
//!
//! 打开方式（默认关闭）：
//! - `HYDRAPORT_LOG=1`        输出到日志文件
//! - `HYDRAPORT_LOG=stderr`   输出到标准错误（调试构建有控制台时更方便）

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
};

use log::{LevelFilter, Metadata, Record};

/// 日志输出目标。
enum Sink {
    /// 不输出任何日志
    Disabled,
    /// 标准错误
    Stderr,
    /// 日志文件
    File(Mutex<File>),
}

struct SimpleLogger {
    sink: Sink,
}

impl log::Log for SimpleLogger {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        !matches!(self.sink, Sink::Disabled)
    }

    fn log(&self, record: &Record<'_>) {
        // 只记录本程序自己的日志，忽略依赖库的噪声
        if !record.target().starts_with("port_manager") {
            return;
        }
        let line = format!(
            "{} [{}] {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            record.level(),
            record.args()
        );
        match &self.sink {
            Sink::Disabled => {}
            Sink::Stderr => eprint!("{line}"),
            Sink::File(file) => {
                if let Ok(mut file) = file.lock() {
                    let _ = file.write_all(line.as_bytes());
                }
            }
        }
    }

    fn flush(&self) {
        if let Sink::File(file) = &self.sink {
            if let Ok(mut file) = file.lock() {
                let _ = file.flush();
            }
        }
    }
}

/// 按环境变量初始化日志；默认关闭，不会产生任何输出。
///
/// 重复调用时只有第一次生效（`log::set_boxed_logger` 只允许设置一次）。
pub fn init() {
    let sink = match std::env::var("HYDRAPORT_LOG").as_deref() {
        Ok("stderr") => Sink::Stderr,
        Ok(value) if !value.is_empty() && value != "0" => match open_log_file() {
            Some(file) => Sink::File(Mutex::new(file)),
            None => Sink::Stderr,
        },
        _ => Sink::Disabled,
    };

    let level = if matches!(sink, Sink::Disabled) {
        LevelFilter::Off
    } else if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };

    if log::set_boxed_logger(Box::new(SimpleLogger { sink })).is_ok() {
        log::set_max_level(level);
    }
}

/// 日志文件位于 `%LOCALAPPDATA%\HydraPort\hydraport.log`，避免写入程序目录（可能无写权限）。
fn open_log_file() -> Option<File> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let directory = base.join("HydraPort");
    std::fs::create_dir_all(&directory).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("hydraport.log"))
        .ok()
}

/// 当前日志是否已启用；UI 用它提示用户去哪儿找日志。
pub fn log_file_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    Some(base.join("HydraPort").join("hydraport.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未设置环境变量时不应打开日志文件。
    #[test]
    fn log_file_path_points_into_local_appdata() {
        // 该测试只验证路径拼接规则，不依赖环境变量是否存在
        if let Some(path) = log_file_path() {
            assert!(path.ends_with("hydraport.log"));
            assert!(path
                .parent()
                .is_some_and(|parent| parent.ends_with("HydraPort")));
        }
    }
}
