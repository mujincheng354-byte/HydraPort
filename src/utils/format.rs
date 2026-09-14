//! 展示用的格式化工具。

/// 把字节数格式化为 MB / GB；数值较小时退回 KB，避免出现 0.0 MB。
pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let value = bytes as f64;
    if value >= GB {
        format!("{:.2} GB", value / GB)
    } else if value >= MB {
        format!("{:.2} MB", value / MB)
    } else {
        format!("{:.1} KB", value / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    /// 内存占用应按量级选择单位，且不出现 0.0 MB 这类无意义结果。
    #[test]
    fn format_bytes_picks_sensible_unit() {
        assert_eq!(format_bytes(512 * 1024), "512.0 KB");
        assert_eq!(format_bytes(64 * 1024 * 1024), "64.00 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }
}
