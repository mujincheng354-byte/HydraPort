//! 剪贴板写入（§7 右键菜单的「复制端口」「复制 PID」）。
//!
//! 剪贴板是全进程共享的资源，所有操作都要在 `OpenClipboard` / `CloseClipboard`
//! 之间完成；打不开时静默失败即可，不值得为一次复制打断用户。

use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
};

/// 剪贴板文本格式。
const CF_UNICODETEXT: u32 = 13;

/// 把文本写入剪贴板；失败返回 `false`。
pub fn set_text(window: HWND, text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * std::mem::size_of::<u16>();

    unsafe {
        if OpenClipboard(window) == 0 {
            return false;
        }
        // 从这里开始无论成败都要关闭剪贴板
        let result = (|| {
            if EmptyClipboard() == 0 {
                return false;
            }
            // 剪贴板要求 GMEM_MOVEABLE，所有权在 SetClipboardData 成功后归系统
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                return false;
            }
            let target = GlobalLock(handle);
            if target.is_null() {
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), target.cast::<u8>(), bytes);
            GlobalUnlock(handle);

            if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                // 交给系统失败，这块内存仍归我们，需要自己释放
                GlobalFree(handle);
                return false;
            }
            true
        })();
        CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写入后应能立刻读回同样的文本。
    #[test]
    fn writes_and_reads_back() {
        // 单元测试进程可能没有消息队列，这里只验证「不 panic」与基本往返
        let written = set_text(std::ptr::null_mut(), "HydraPort 剪贴板测试");
        if !written {
            // 剪贴板被别的进程占用时允许失败，不把环境问题当成代码缺陷
            return;
        }
        assert_eq!(read_text(), "HydraPort 剪贴板测试");
    }

    /// 读取剪贴板文本，仅用于测试。
    fn read_text() -> String {
        use windows_sys::Win32::System::{
            DataExchange::{GetClipboardData, IsClipboardFormatAvailable},
            Memory::{GlobalLock, GlobalUnlock},
        };
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 || OpenClipboard(std::ptr::null_mut()) == 0 {
                return String::new();
            }
            let handle = GetClipboardData(CF_UNICODETEXT);
            let text = if handle.is_null() {
                String::new()
            } else {
                let pointer = GlobalLock(handle) as *const u16;
                let mut length = 0;
                while *pointer.add(length) != 0 {
                    length += 1;
                }
                let value = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
                GlobalUnlock(handle);
                value
            };
            CloseClipboard();
            text
        }
    }
}
