//! 系统「另存为」对话框（§1.2 导出 CSV）。
//!
//! 用 comdlg32 的 `GetSaveFileNameW` 而不是第三方对话框库：不引入依赖，
//! 外观与系统一致，也不需要初始化 COM。

use std::path::PathBuf;

use windows_sys::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_EXPLORER, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};

/// 弹出「另存为」对话框；用户取消时返回 `None`。
pub fn save_file_dialog(default_name: &str) -> Option<PathBuf> {
    // 过滤器字符串的格式是「说明\0通配符\0说明\0通配符\0\0」，以双重 NUL 结尾
    let filter: Vec<u16> = "CSV 文件\0*.csv\0所有文件\0*.*\0\0".encode_utf16().collect();
    let extension: Vec<u16> = "csv\0".encode_utf16().collect();

    // 缓冲区在返回时会被填入完整路径，必须先放入默认文件名
    let mut buffer = vec![0u16; 1024];
    for (slot, value) in buffer.iter_mut().zip(default_name.encode_utf16()) {
        *slot = value;
    }

    let mut options: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    options.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    options.lpstrFilter = filter.as_ptr();
    options.lpstrFile = buffer.as_mut_ptr();
    options.nMaxFile = buffer.len() as u32;
    options.lpstrDefExt = extension.as_ptr();
    options.Flags = OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST;

    let saved = unsafe { GetSaveFileNameW(&mut options) } != 0;
    if !saved {
        return None;
    }

    let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    if length == 0 {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
}
