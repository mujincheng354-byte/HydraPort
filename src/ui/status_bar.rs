//! 底部状态栏：端口总数、选中数、操作提示、最后刷新时间（§1.1.7）。
//!
//! 状态栏是少数没有深色皮肤的系统控件——`DarkMode_Explorer` 对它无效，深色主题下
//! 它会固执地留成白底黑字。所以深色时分段改成自绘（`SBT_OWNERDRAW`），由主窗口的
//! `WM_DRAWITEM` 用当前主题的配色画出来；浅色时仍走系统默认外观。

use std::ptr;

use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        DrawTextW, FillRect, SetBkMode, SetTextColor, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
        DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            DRAWITEMSTRUCT, SBARS_SIZEGRIP, SBT_OWNERDRAW, SB_SETPARTS, SB_SETTEXTW,
            STATUSCLASSNAMEW,
        },
        WindowsAndMessaging::{CreateWindowExW, SendMessageW, WS_CHILD, WS_VISIBLE},
    },
};

use crate::app::PortManagerApp;
use crate::ui::{self, ids, to_wide};

/// 状态栏分三段：统计信息 / 操作提示 / 最后刷新时间。
pub(crate) const PART_COUNT: usize = 3;

/// 各段分界线的百分比位置；最后一段用 -1 表示「一直到右边界」。
const PART_EDGES: [i32; PART_COUNT] = [45, 80, -1];

/// 绘制分段时文字距左边界的内边距。
const TEXT_INSET: i32 = 4;

/// 创建状态栏。
///
/// 状态栏承载提示信息（含非管理员时的提权提示），缺了它界面仍然是可用的，
/// 但用户会看不到任何反馈。这里同样按失败处理：与其留一个哑掉的界面，
/// 不如启动时就说清楚是状态栏没建起来。
pub(crate) fn create(parent: HWND) -> anyhow::Result<HWND> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let status = unsafe {
        CreateWindowExW(
            0,
            STATUSCLASSNAMEW,
            ptr::null(),
            WS_CHILD | WS_VISIBLE | SBARS_SIZEGRIP,
            0,
            0,
            0,
            0,
            parent,
            ids::STATUS_BAR as _,
            instance,
            ptr::null(),
        )
    };
    if status.is_null() {
        anyhow::bail!("创建状态栏失败：{}", std::io::Error::last_os_error());
    }

    apply_edges(status, parent);
    Ok(status)
}

/// 按主窗口客户区宽度重算三段的分界；每次布局都要调，否则窗口缩放后分段不会跟着变。
pub(crate) fn resize(app: &PortManagerApp) {
    apply_edges(app.status, app.window);
}

/// 分段宽度按客户区宽度等比例分配。
fn apply_edges(status: HWND, parent: HWND) {
    let (width, _) = ui::client_size(parent);
    let mut edges = [0i32; PART_COUNT];
    for (slot, percent) in edges.iter_mut().zip(PART_EDGES) {
        *slot = if percent < 0 {
            -1
        } else {
            width * percent / 100
        };
    }
    unsafe { SendMessageW(status, SB_SETPARTS, PART_COUNT, edges.as_ptr() as isize) };
}

/// 状态栏当前高度。
pub(crate) fn height(status: HWND) -> i32 {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(status, &mut rect) };
    rect.bottom - rect.top
}

/// 按当前状态刷新三段文本。
pub(crate) fn refresh(app: &mut PortManagerApp) {
    let first = format!(
        "共 {} 条，显示 {} 条    已选中 {} 项",
        app.ports.len(),
        app.rows.len(),
        app.selected.len()
    );
    let second = if app.rows.is_empty() && !app.ports.is_empty() {
        "没有匹配的监听端口".to_owned()
    } else {
        app.message.clone()
    };
    let third = match app.last_refresh {
        Some(time) => format!("上次刷新：{}", time.format("%H:%M:%S")),
        None => "尚未刷新".to_owned(),
    };
    app.status_parts = [first, second, third];

    for (index, text) in app.status_parts.iter().enumerate() {
        let mut wide = to_wide(text);
        // 自绘分段的文本不经过系统，只能靠 itemData 把段号带给 WM_DRAWITEM
        let (wparam, lparam) = if app.dark_mode {
            (index | SBT_OWNERDRAW as usize, index as isize)
        } else {
            (index, wide.as_mut_ptr() as isize)
        };
        unsafe { SendMessageW(app.status, SB_SETTEXTW, wparam, lparam) };
    }
}

/// 画出一个自绘分段；由主窗口的 `WM_DRAWITEM` 调用。
pub(crate) fn draw_item(app: &PortManagerApp, item: &DRAWITEMSTRUCT) {
    let dc = item.hDC;
    if dc.is_null() {
        return;
    }
    let index = item.itemID as usize;
    let Some(text) = app.status_parts.get(index) else {
        return;
    };

    let mut wide = to_wide(text);
    let mut rect = item.rcItem;
    rect.left += ui::scale(TEXT_INSET, app.dpi);
    unsafe {
        FillRect(dc, &item.rcItem, app.background);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(
            dc,
            if app.dark_mode {
                ui::COLOR_DARK_TEXT
            } else {
                ui::COLOR_LIGHT_TEXT
            },
        );
        DrawTextW(
            dc,
            wide.as_mut_ptr(),
            -1,
            &mut rect,
            // DT_NOPREFIX：提示语里可能有 & 之类会被当成快捷键前缀的字符
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
}
