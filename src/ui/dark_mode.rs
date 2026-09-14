//! 深色控件主题的进程级开关（§1.2）。
//!
//! 只把控件窗口的 `SetWindowTheme` 换成 `DarkMode_Explorer` 是不够的：进程没有
//! 切到「允许深色」模式时，系统会直接忽略这些主题名——表现就是标题栏已经变黑，
//! 列表与状态栏却仍是白底黑字。切换模式只有 uxtheme.dll 的两个未公开序号导出能
//! 做到（135 `SetPreferredAppMode`、133 `AllowDarkModeForWindow`）。
//!
//! 它们从 Windows 10 1809 起一直存在、行为稳定，但既然是未公开接口，这里全程按
//! 「可能取不到」处理：取不到就静默降级（控件维持系统默认外观），绝不 panic。

use std::{ffi::c_void, sync::OnceLock};

use windows_sys::Win32::{
    Foundation::HWND,
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};

use super::to_wide;

/// uxtheme.dll 里的未公开导出序号。
const SET_PREFERRED_APP_MODE: u16 = 135;
const ALLOW_DARK_MODE_FOR_WINDOW: u16 = 133;

/// `PreferredAppMode::AllowDark`：跟随系统设置，而不是强制深色。
const ALLOW_DARK: i32 = 1;

type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
type AllowDarkModeForWindow = unsafe extern "system" fn(HWND, i32) -> i32;

/// 解析出来的两个入口；任一个缺失就是 `None`。
struct Api {
    set_preferred_app_mode: Option<SetPreferredAppMode>,
    allow_dark_mode_for_window: Option<AllowDarkModeForWindow>,
}

/// 按序号取函数地址；`GetProcAddress` 认序号的方式是「指针的低 16 位就是序号」。
unsafe fn resolve<T: Copy>(module: *mut c_void, ordinal: u16) -> Option<T> {
    let address = GetProcAddress(module, ordinal as usize as *const u8)?;
    // 双方都是函数指针、长度一致；泛型参数下 `transmute` 无法通过编译，只能用它
    Some(std::mem::transmute_copy(&address))
}

/// 解析一次后缓存。`LoadLibraryW` 对同一模块只增加引用计数，不会重复加载。
fn api() -> &'static Api {
    static API: OnceLock<Api> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let name = to_wide("uxtheme.dll");
        let module = LoadLibraryW(name.as_ptr());
        if module.is_null() {
            log::warn!("未能加载 uxtheme.dll，控件将不跟随深色主题");
            return Api {
                set_preferred_app_mode: None,
                allow_dark_mode_for_window: None,
            };
        }
        Api {
            set_preferred_app_mode: resolve(module, SET_PREFERRED_APP_MODE),
            allow_dark_mode_for_window: resolve(module, ALLOW_DARK_MODE_FOR_WINDOW),
        }
    })
}

/// 切到「允许深色」模式。必须在创建任何控件之前调用，已创建的窗口不会补上效果。
pub(crate) fn enable_for_process() {
    match api().set_preferred_app_mode {
        Some(set_mode) => {
            unsafe { set_mode(ALLOW_DARK) };
        }
        None => log::warn!("系统未提供 SetPreferredAppMode，控件将不跟随深色主题"),
    }
}

/// 允许（深色）或不允许（浅色）某个窗口使用深色主题。
pub(crate) fn allow_for_window(window: HWND, dark: bool) {
    if let Some(allow) = api().allow_dark_mode_for_window {
        unsafe { allow(window, i32::from(dark)) };
    }
}
