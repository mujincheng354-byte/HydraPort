//! 界面层（§3）：全部使用原生 Win32 控件。
//!
//! 各子模块只负责一类控件的创建与消息处理，业务动作仍然通过
//! `impl PortManagerApp` 的方法完成——服务层不依赖界面层，界面层也只经由 app
//! 触碰业务（§12）。控件句柄统一挂在 [`crate::app::PortManagerApp`] 上，
//! 窗口过程通过 `GWLP_USERDATA` 取回它。

pub(crate) mod dark_mode;
pub(crate) mod dialogs;
pub(crate) mod main_window;
pub(crate) mod status_bar;
pub(crate) mod table;
pub(crate) mod toolbar;

pub(crate) use main_window::run;

use std::ptr;

use windows_sys::Win32::{
    Foundation::{HWND, RECT, SIZE},
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::{CreateFontIndirectW, CreateSolidBrush, DeleteObject, HBRUSH, HFONT},
    },
    UI::{
        Controls::{SetWindowTheme, LVM_GETHEADER},
        HiDpi::GetDpiForWindow,
        WindowsAndMessaging::{
            GetClientRect, GetWindowTextLengthW, GetWindowTextW, SendMessageW, SetWindowTextW,
            SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, WM_SETFONT,
        },
    },
};

/// 托盘事件投递到主窗口的自定义消息；`wparam` 为事件编号（见 [`crate::services::TrayEvent`]）。
pub(crate) const WM_TRAY_EVENT: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// 控件的菜单项 ID。`WM_COMMAND` 的低 16 位就是它。
pub(crate) mod ids {
    pub(crate) const SEARCH_EDIT: usize = 1001;
    pub(crate) const EXACT_EDIT: usize = 1002;
    pub(crate) const QUERY_BUTTON: usize = 1003;
    pub(crate) const REFRESH_BUTTON: usize = 1004;
    pub(crate) const EXPORT_BUTTON: usize = 1005;
    pub(crate) const KILL_BUTTON: usize = 1006;
    pub(crate) const THEME_BUTTON: usize = 1007;
    pub(crate) const SETTINGS_BUTTON: usize = 1008;
    pub(crate) const LIST_VIEW: usize = 1009;
    pub(crate) const STATUS_BAR: usize = 1010;
    /// 两个静态标签的 ID 只用于创建窗口，布局时靠句柄数组取回
    pub(crate) const SEARCH_LABEL: usize = 1011;
    pub(crate) const EXACT_LABEL: usize = 1012;

    /// 「设置」下拉菜单
    pub(crate) const MENU_CLOSE_TO_TRAY: usize = 2001;
    pub(crate) const MENU_AUTOSTART: usize = 2002;
    pub(crate) const MENU_COPY_LOG_PATH: usize = 2003;

    /// 表格右键菜单
    pub(crate) const MENU_COPY_PORT: usize = 2010;
    pub(crate) const MENU_COPY_PID: usize = 2011;
    pub(crate) const MENU_DETAIL: usize = 2012;
    pub(crate) const MENU_OPEN_LOCATION: usize = 2013;
    pub(crate) const MENU_KILL: usize = 2014;
}

/// 深色 / 浅色主题用到的颜色。`COLORREF` 的字节顺序是 0x00BBGGRR。
pub(crate) const COLOR_LIGHT_BG: u32 = 0x00FF_FFFF;
pub(crate) const COLOR_LIGHT_TEXT: u32 = 0x0000_0000;
pub(crate) const COLOR_DARK_BG: u32 = 0x0020_2020;
pub(crate) const COLOR_DARK_TEXT: u32 = 0x00E0_E0E0;

/// 转成以 NUL 结尾的 UTF-16。
pub(crate) fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读取控件文本；失败时返回空串。
pub(crate) fn control_text(window: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(window);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let written = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
    }
}

/// 设置控件文本。
pub(crate) fn set_control_text(window: HWND, text: &str) {
    let wide = to_wide(text);
    unsafe { SetWindowTextW(window, wide.as_ptr()) };
}

/// 窗口所在显示器的 DPI；拿不到时按 96 处理。
pub(crate) fn dpi_of(window: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(window) };
    if dpi == 0 {
        96
    } else {
        dpi
    }
}

/// 把逻辑像素（96 DPI 下的值）换算成当前 DPI 的物理像素。
pub(crate) fn scale(value: i32, dpi: u32) -> i32 {
    (value as i64 * dpi as i64 / 96) as i32
}

/// 用给定字体测量文本宽度；工具栏靠它算控件尺寸，从而在任何 DPI 下都不裁字。
pub(crate) fn text_width(window: HWND, font: HFONT, text: &str) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::{
        GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject,
    };
    unsafe {
        let dc = GetDC(window);
        if dc.is_null() {
            return 0;
        }
        let previous = SelectObject(dc, font as *mut _);
        let wide = to_wide(text);
        let mut size: SIZE = std::mem::zeroed();
        // 末尾的 NUL 不算宽度
        GetTextExtentPoint32W(dc, wide.as_ptr(), wide.len() as i32 - 1, &mut size);
        SelectObject(dc, previous);
        ReleaseDC(window, dc);
        size.cx
    }
}

/// 客户区尺寸。
pub(crate) fn client_size(window: HWND) -> (i32, i32) {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(window, &mut rect) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// 按系统消息字体创建界面字体（含中文字形，无需自己加载字体文件）。
pub(crate) fn create_ui_font(dpi: u32) -> HFONT {
    unsafe {
        let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
        metrics.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
        if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            &mut metrics as *mut _ as *mut _,
            0,
        ) == 0
        {
            return ptr::null_mut();
        }
        // lfMessageFont 是按 96 DPI 给出的，需要按当前 DPI 缩放字高
        let mut log_font = metrics.lfMessageFont;
        log_font.lfHeight = scale(log_font.lfHeight, dpi);
        CreateFontIndirectW(&log_font)
    }
}

/// 给窗口及其所有子控件换字体。
pub(crate) fn apply_font(window: HWND, font: HFONT) {
    unsafe { SendMessageW(window, WM_SETFONT, font as usize, 1) };
    for child in children_of(window) {
        apply_font(child, font);
    }
}

/// 直接子控件句柄列表。
pub(crate) fn children_of(window: HWND) -> Vec<HWND> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindow, GW_CHILD, GW_HWNDNEXT};
    let mut result = Vec::new();
    unsafe {
        let mut child = GetWindow(window, GW_CHILD);
        while !child.is_null() {
            result.push(child);
            child = GetWindow(child, GW_HWNDNEXT);
        }
    }
    result
}

/// 给窗口套上 uxtheme 主题名。
fn set_window_theme(window: HWND, theme: &str) {
    let theme = to_wide(theme);
    unsafe { SetWindowTheme(window, theme.as_ptr(), ptr::null()) };
}

/// 切换深色 / 浅色外观。
///
/// 原生控件的深色模式没有统一接口：标题栏走 DWM 属性，列表视图与编辑框靠
/// uxtheme 的 DarkMode_* 主题（且必须先由 [`dark_mode::enable_for_process`] 把进程
/// 切到允许深色的模式，否则这些主题名会被忽略），静态文本与背景色则要自己响应
/// `WM_CTLCOLORSTATIC`。
pub(crate) fn apply_theme(window: HWND, dark: bool) {
    unsafe {
        // DWMWA_USE_IMMERSIVE_DARK_MODE = 20（Windows 10 1809 起）
        let value: i32 = if dark { 1 } else { 0 };
        DwmSetWindowAttribute(
            window,
            20,
            &value as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
    dark_mode::allow_for_window(window, dark);

    let list_theme = if dark {
        "DarkMode_Explorer"
    } else {
        "Explorer"
    };
    let edit_theme = if dark { "DarkMode_CFD" } else { "Explorer" };
    for child in children_of(window) {
        let class = class_name_of(child);
        if class == "msctls_statusbar32" {
            // 状态栏要单独走：给它套主题反而会让它忽略颜色消息（见 set_colors 的说明），
            // 所以这里不参与下面的主题名分配，由 set_colors 自己摘主题再上色
            status_bar::set_colors(child, dark);
            continue;
        }
        let theme = match class.as_str() {
            "SysListView32" => {
                // 表头是列表视图的子窗口，不单独换主题的话它会留成一道白条。
                // 但换主题对它没用（实测：SysHeader32 既不认 DarkMode_Explorer 也不发
                // NM_CUSTOMDRAW），真正的深色得靠子类化自己画，见 table::install_header_subclass。
                let header = unsafe { SendMessageW(child, LVM_GETHEADER, 0, 0) } as HWND;
                if !header.is_null() {
                    dark_mode::allow_for_window(header, dark);
                    set_window_theme(header, list_theme);
                }
                table::install_header_subclass(child);
                table::set_header_dark(child, dark);
                // 列表视图背景色不跟 uxtheme 走，必须显式设置
                table::set_list_colors(child, dark);
                list_theme
            }
            "Edit" => edit_theme,
            // 按钮没有官方深色主题，保持系统默认外观
            _ => continue,
        };
        dark_mode::allow_for_window(child, dark);
        set_window_theme(child, theme);
    }
}

/// 窗口类名。
pub(crate) fn class_name_of(window: HWND) -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buffer = [0u16; 64];
    let length = unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

/// 当前主题下的窗口背景画刷；调用方负责在用完后 `DeleteObject`。
pub(crate) fn create_background_brush(dark: bool) -> HBRUSH {
    unsafe { CreateSolidBrush(if dark { COLOR_DARK_BG } else { COLOR_LIGHT_BG }) }
}

/// 删除 GDI 对象；空句柄直接忽略。
pub(crate) fn delete_object(object: *mut core::ffi::c_void) {
    if !object.is_null() {
        unsafe { DeleteObject(object) };
    }
}
