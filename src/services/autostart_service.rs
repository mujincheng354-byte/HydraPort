//! 开机自启（§1.2 可选项）：读写当前用户的 `Run` 注册表项。
//!
//! 只写 `HKEY_CURRENT_USER`，不碰 `HKEY_LOCAL_MACHINE`，因此不需要管理员权限，
//! 也不会影响其他用户；关闭开关时会删除对应的值，不留残余。

use std::{path::Path, ptr};

use anyhow::{anyhow, Result};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, REG_SAM_FLAGS,
    },
};

/// 自启动项的注册表路径与值名。
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "HydraPort";

/// 负责开机自启的查询与开关。
pub struct AutostartService;

impl AutostartService {
    /// 当前是否已经设置了指向本程序的自启动项。
    pub fn is_enabled() -> bool {
        read_command(VALUE_NAME)
            .map(|command| matches_current_exe(&command))
            .unwrap_or(false)
    }

    /// 打开或关闭开机自启；写入后立即回读校验，避免「看起来成功实际没写进去」。
    pub fn set_enabled(enabled: bool) -> Result<()> {
        if enabled { Self::enable() } else { Self::disable() }
    }

    fn enable() -> Result<()> {
        let exe = std::env::current_exe().map_err(|error| anyhow!("无法获取程序路径：{error}"))?;
        // 路径可能包含空格，必须加引号，否则系统会把空格前的部分当成程序名
        write_command(VALUE_NAME, &format!("\"{}\"", exe.display()))?;
        if !Self::is_enabled() {
            return Err(anyhow!("自启动项写入后校验失败，请检查注册表权限"));
        }
        Ok(())
    }

    fn disable() -> Result<()> {
        delete_value(VALUE_NAME)
    }
}

/// 读取指定值登记的命令行；未设置时返回 `None`。
fn read_command(value_name: &str) -> Option<String> {
    let key = open_key(KEY_QUERY_VALUE).ok()?;
    let name = to_wide(value_name);
    let mut size = 0_u32;
    // 第一次调用只取所需字节数
    let probe = unsafe { RegQueryValueExW(key, name.as_ptr(), ptr::null(), ptr::null_mut(), ptr::null_mut(), &mut size) };
    if probe != ERROR_SUCCESS || size == 0 {
        unsafe { RegCloseKey(key); }
        return None;
    }

    // size 是字节数，按 UTF-16 换算成 u16 个数后再多留一个位置给结尾 NUL
    let mut buffer = vec![0_u16; size as usize / 2 + 1];
    let result = unsafe {
        RegQueryValueExW(key, name.as_ptr(), ptr::null(), ptr::null_mut(), buffer.as_mut_ptr().cast::<u8>(), &mut size)
    };
    unsafe { RegCloseKey(key); }
    if result != ERROR_SUCCESS {
        return None;
    }

    let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// 写入自启动命令。
fn write_command(value_name: &str, command: &str) -> Result<()> {
    let key = open_key(KEY_SET_VALUE)?;
    let name = to_wide(value_name);
    // REG_SZ 需要包含结尾的 NUL，长度按字节计算
    let value = to_wide(command);
    let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2) };
    let result = unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, bytes.as_ptr(), bytes.len() as u32) };
    unsafe { RegCloseKey(key); }
    if result == ERROR_SUCCESS { Ok(()) } else { Err(anyhow!("写入自启动项失败，错误代码：{result}")) }
}

/// 删除自启动项；本来就不存在也算成功。
fn delete_value(value_name: &str) -> Result<()> {
    let key = open_key(KEY_SET_VALUE)?;
    let name = to_wide(value_name);
    let result = unsafe { RegDeleteValueW(key, name.as_ptr()) };
    unsafe { RegCloseKey(key); }
    if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(anyhow!("删除自启动项失败，错误代码：{result}"))
    }
}

fn open_key(access: REG_SAM_FLAGS) -> Result<HKEY> {
    let path = to_wide(RUN_KEY);
    let mut key: HKEY = ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key) };
    if result == ERROR_SUCCESS { Ok(key) } else { Err(anyhow!("打开注册表 Run 项失败，错误代码：{result}")) }
}

/// 判断登记的命令行是否指向当前可执行文件。
fn matches_current_exe(command: &str) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    paths_equal(Path::new(command.trim().trim_matches('"')), &exe)
}

/// 按 Windows 的规则比较路径：忽略大小写，并把 `/` 与 `\` 视为等价。
fn paths_equal(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| path.to_string_lossy().replace('/', "\\").to_lowercase();
    normalize(left) == normalize(right)
}

/// 转成以 NUL 结尾的 UTF-16。
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用值名：绝不碰正式的自启动项，避免测试中途失败时把开发机的自启动改坏。
    const TEST_VALUE: &str = "HydraPortSelfTest";

    /// 离开作用域时清理测试值，即使断言失败也不会留下残余。
    struct Cleanup;

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = delete_value(TEST_VALUE);
        }
    }

    /// 路径比较应忽略大小写与斜杠方向。
    #[test]
    fn path_comparison_is_case_and_separator_insensitive() {
        assert!(paths_equal(Path::new("C:\\App\\Port.exe"), Path::new("c:/app/port.EXE")));
        assert!(!paths_equal(Path::new("C:\\App\\Port.exe"), Path::new("C:\\App\\Other.exe")));
    }

    /// 注册表值名的编码应以 NUL 结尾。
    #[test]
    fn wide_string_is_nul_terminated() {
        // "HydraPort" 共 9 个字符，加上结尾的 NUL 共 10 个 u16
        let wide = to_wide("HydraPort");
        assert_eq!(wide.len(), 10);
        assert_eq!(wide[9], 0);
    }

    /// 注册表读写往返：写入后能读回，删除后读不到。
    #[test]
    fn write_read_delete_round_trip() {
        let _cleanup = Cleanup;
        let _ = delete_value(TEST_VALUE);
        assert!(read_command(TEST_VALUE).is_none(), "清理后不应读到测试值");

        // 含空格与中文的路径必须原样保存，不能被截断
        let command = "\"C:\\Program Files\\端口管理工具\\port-manager.exe\"";
        write_command(TEST_VALUE, command).expect("写入测试值失败");
        assert_eq!(read_command(TEST_VALUE).as_deref(), Some(command), "读回的命令与写入不一致");

        delete_value(TEST_VALUE).expect("删除测试值失败");
        assert!(read_command(TEST_VALUE).is_none(), "删除后不应再读到测试值");
        // 重复删除应当幂等
        delete_value(TEST_VALUE).expect("重复删除不应报错");
    }

    /// 正式自启动项与当前可执行文件的比较：测试程序的路径里面必然含有 port_manager。
    #[test]
    fn matches_current_exe_recognizes_own_path() {
        let exe = std::env::current_exe().expect("无法获取当前可执行文件路径");
        assert!(matches_current_exe(&exe.display().to_string()));
        assert!(matches_current_exe(&format!("\"{}\"", exe.display())), "带引号的路径也应识别");
        assert!(!matches_current_exe("C:\\Windows\\System32\\notepad.exe"));
    }
}
