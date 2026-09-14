//! 系统主题检测（§1.2 深色 / 浅色主题切换）。
//!
//! Windows 把「应用使用浅色主题」记在注册表里：
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` 的
//! `AppsUseLightTheme`（DWORD，0 表示深色）。读不到时按浅色处理。

use std::ptr;

use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    },
};

const KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const VALUE_NAME: &str = "AppsUseLightTheme";

/// 系统当前是否使用深色主题。
pub fn system_uses_dark_theme() -> bool {
    match read_apps_use_light_theme() {
        // 值不存在时（老系统）默认浅色
        Some(light) => light == 0,
        None => false,
    }
}

/// 读取 `AppsUseLightTheme`；键或值不存在时返回 `None`。
fn read_apps_use_light_theme() -> Option<u32> {
    let path: Vec<u16> = KEY_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let name: Vec<u16> = VALUE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut key: HKEY = ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) != ERROR_SUCCESS {
            return None;
        }

        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let result = RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut value as *mut u32 as *mut u8,
            &mut size,
        );
        RegCloseKey(key);

        (result == ERROR_SUCCESS && size == std::mem::size_of::<u32>() as u32).then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读取结果只能是 0 或 1；键不存在时应当返回 `None` 而不是 panic。
    #[test]
    fn reads_a_plausible_value() {
        if let Some(value) = read_apps_use_light_theme() {
            assert!(
                value <= 1,
                "AppsUseLightTheme 只应是 0 或 1，实际为 {value}"
            );
        }
        // 无论读不读得到，都必须能给出一个确定答案
        let _ = system_uses_dark_theme();
    }
}
