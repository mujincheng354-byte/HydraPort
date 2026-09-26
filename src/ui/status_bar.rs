//! 底部状态栏：端口总数、选中数、操作提示、最后刷新时间（§1.1.7）。
//!
//! 状态栏是系统控件里最不配合深色主题的一个：`DarkMode_Explorer` 对它无效，
//! `SB_SETBKCOLOR` 在它套着主题时会被忽略，而文字颜色根本没有对应的消息
//! （`CCM_SETTEXTCOLOR` 不是状态栏的，发过去石沉大海）。所以这里两条路一起走：
//! 摘掉主题让 `SB_SETBKCOLOR` 生效、把整条底色定下来；三个分段再各自自绘
//! （`SBT_OWNERDRAW`），文字颜色才能真正跟着主题走。深浅两套都自绘，浅色下画出来
//! 的结果与系统默认外观一致。
//!
//! 还有一处是自绘也够不着的：comctl32 会在状态栏右端留出约一个滚动条宽的非分段区
//! （尺寸柄的位置）并在顶部留 2px 立体边，`DRAWITEMSTRUCT::rcItem` 两者都不含，
//! 系统拿 `COLOR_BTNFACE` 自己画，`SB_SETBKCOLOR` 也管不到——深色主题下右下角就是
//! 一条白。所以填充走 [`part_rect`] 自己算的整段矩形，而不是 rcItem。

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{
        CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetDC, GetSysColor, GetSysColorBrush,
        InvalidateRect, ReleaseDC, SetBkMode, SetTextColor, COLOR_BTNFACE, COLOR_BTNTEXT,
        DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            SetWindowTheme, DRAWITEMSTRUCT, SBT_OWNERDRAW, SB_GETRECT, SB_SETBKCOLOR, SB_SETPARTS,
            SB_SETTEXTW, STATUSCLASSNAMEW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, GetWindowLongPtrW, SendMessageW, SetWindowLongPtrW, GWLP_WNDPROC,
            WM_PAINT, WS_CHILD, WS_VISIBLE,
        },
    },
};

use crate::app::PortManagerApp;
use crate::ui::{self, ids, to_wide};

/// 状态栏分三段：统计信息 / 操作提示 / 最后刷新时间。
pub(crate) const PART_COUNT: usize = 3;

/// 各段分界线的百分比位置；最后一段用 -1 表示「一直到右边界」，由 [`part_edges`] 换算。
const PART_EDGES: [i32; PART_COUNT] = [45, 80, -1];

/// 绘制分段时文字距左边界的内边距。
const TEXT_INSET: i32 = 4;

/// 系统那条立体分隔线占的宽度：边界两侧各 1px。
const SYSTEM_BORDER_BAND: i32 = 2;

/// 深色主题下补画的分隔线颜色，与工具栏边框取同一个灰。
const DARK_SEPARATOR: u32 = 0x0050_5050;

/// 创建状态栏。
///
/// 不加 `SBARS_SIZEGRIP`：系统那个尺寸柄只有浅色一套画法，深浅两色都画不出深色主题
/// 该有的样子，而且它在我们的自绘之后补画，盖不住。窗口本身就是可缩放边框，少了它
/// 没有功能损失。
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
            WS_CHILD | WS_VISIBLE,
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

    Ok(status)
}

/// 按状态栏当前宽度重算三段的分界；每次布局都要调，否则窗口缩放后分段不会跟着变。
pub(crate) fn resize(app: &PortManagerApp) {
    apply_edges(app.status);
}

/// 当前主题下的状态栏底色。
fn background_color(dark: bool) -> u32 {
    if dark {
        ui::COLOR_DARK_BG
    } else {
        // 浅色沿用系统按钮面色（#F0F0F0），与经典状态栏的外观一致
        unsafe { GetSysColor(COLOR_BTNFACE) }
    }
}

/// 当前主题下的状态栏文字色。
fn text_color(dark: bool) -> u32 {
    if dark {
        ui::COLOR_DARK_TEXT
    } else {
        unsafe { GetSysColor(COLOR_BTNTEXT) }
    }
}

/// 定下状态栏整条的底色。
///
/// 关键在于**先摘掉主题**：套着 comctl32 v6 主题时 `SB_SETBKCOLOR` 会被完全忽略
/// （实测深色主题下它照样是 #F0F0F0 白底黑字）。把主题名设成空串让它退回经典绘制，
/// 这个颜色才会生效。代价是少掉 v6 主题那条顶部立体边，换来整条状态栏真的能变色。
/// 文字色没有对应的状态栏消息，只能由 [`draw_item`] 自绘时上色。
pub(crate) fn set_colors(status: HWND, dark: bool) {
    let empty = to_wide("");
    unsafe {
        SetWindowTheme(status, empty.as_ptr(), empty.as_ptr());
        SendMessageW(status, SB_SETBKCOLOR, 0, background_color(dark) as isize);
    }
    install_subclass(status, dark);
    unsafe {
        // 颜色消息只影响后续绘制，已经画好的一条要显式作废
        InvalidateRect(status, ptr::null(), 1);
    }
}

/// 状态栏被替换下来的原窗口过程（0 表示尚未挂钩）。
static STATUS_ORIGINAL: AtomicIsize = AtomicIsize::new(0);

/// 当前是否深色；窗口过程拿不到 `PortManagerApp`（`GWLP_USERDATA` 归主窗口），
/// 只能放在单元级静态里，与表头的做法一致。
static STATUS_DARK: AtomicBool = AtomicBool::new(false);

/// 装状态栏子类化。只做一次，重复调用只更新深浅开关。
///
/// 子类化是为了补上自绘够不着的那一条：分段矩形由 comctl32 给出，而它在右端留了
/// 约一个滚动条宽的非分段区（`SB_GETRECT` 实测最后一段右边界比客户区窄 27px @150%），
/// `DRAWITEMSTRUCT` 的 DC 又按分段矩形裁剪，`FillRect` 铺过去也会被剪掉。这块由系统
/// 用 `COLOR_BTNFACE` 自己画，深色主题下就是右下角一条白。所以这里挂上窗口过程，
/// 在原过程画完之后照着实测的差值把这条补上。
fn install_subclass(status: HWND, dark: bool) {
    STATUS_DARK.store(dark, Ordering::Relaxed);
    if STATUS_ORIGINAL.load(Ordering::Relaxed) != 0 {
        return;
    }
    unsafe {
        let original = GetWindowLongPtrW(status, GWLP_WNDPROC);
        if original == 0 {
            log::warn!("取状态栏原窗口过程失败，深色主题下右下角会留一条浅色");
            return;
        }
        STATUS_ORIGINAL.store(original, Ordering::Relaxed);
        SetWindowLongPtrW(status, GWLP_WNDPROC, status_procedure as *const () as isize);
    }
}

/// 状态栏的替代窗口过程：原过程照常跑，跑完再补画它让出来的那一条。
///
/// 顺序不能反：那块浅色正是原过程画的，画在它前面等于没画。
unsafe extern "system" fn status_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let raw = STATUS_ORIGINAL.load(Ordering::Relaxed);
    if raw == 0 {
        return 0;
    }
    // `WNDPROC` 在 windows-sys 里是 `Option<unsafe extern "system" fn ...>`，
    // `Option<fn>` 有空指针优化，非 0 地址一定有值。
    let original = match std::mem::transmute::<
        isize,
        Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    >(raw)
    {
        Some(procedure) => procedure,
        None => return 0,
    };

    let result = original(window, message, wparam, lparam);
    // 浅色下那些线本来就是对的，补画反而会抹掉系统的尺寸柄提示
    if message == WM_PAINT && STATUS_DARK.load(Ordering::Relaxed) {
        paint_system_borders(window);
    }
    result
}

/// 盖掉系统画死的那几条浅色边，再按主题补上自己的分隔线。
///
/// comctl32 在控件左边缘和每个分段边界上各画一条 2px 的立体分隔线，颜色取自经典的
/// 浅色系统色（实测 `#FFFFFF` / `#A0A0A0`，最后一段边界那条是 `#F0F0F0`）。它们画在
/// 自绘分段**之后**，`SB_SETBKCOLOR` 也管不到，深色主题下就是一条条横贯状态栏的浅色
/// 竖线。最后一段右侧那块尺寸柄保留区同理。这些都没有深色版本，只能等原过程画完
/// 之后自己盖掉，再按主题画回分隔线——分段边界本身是有用的视觉信息，不该一并抹平。
///
/// 边界位置：分段边界用我们自己算的 [`part_edges`]（和自绘填色同源），右侧保留区
/// 问 `SB_GETRECT`——它随 DPI 变，而且 `SB_SETPARTS` 传多少都不影响它，只有系统
/// 自己的答案才准。
unsafe fn paint_system_borders(window: HWND) {
    let (client_width, client_height) = ui::client_size(window);
    if client_width <= 0 || client_height <= 0 {
        return;
    }
    let mut last: RECT = std::mem::zeroed();
    if SendMessageW(
        window,
        SB_GETRECT,
        PART_COUNT - 1,
        &mut last as *mut _ as isize,
    ) == 0
    {
        return;
    }

    let hdc = GetDC(window);
    if hdc.is_null() {
        return;
    }
    let background = CreateSolidBrush(background_color(true));
    let separator = CreateSolidBrush(DARK_SEPARATOR);
    let band = |left: i32, right: i32| RECT {
        left: left.max(0),
        top: 0,
        right: right.min(client_width),
        bottom: client_height,
    };

    // 控件左边缘那条
    FillRect(hdc, &band(0, SYSTEM_BORDER_BAND), background);
    // 分段边界：系统画在边界两侧各 1px，两边再各留 1px 余量
    let edges = part_edges(client_width);
    for edge in edges.iter().take(PART_COUNT - 1) {
        FillRect(
            hdc,
            &band(edge - SYSTEM_BORDER_BAND, edge + SYSTEM_BORDER_BAND + 1),
            background,
        );
    }
    // 最后一段右侧的保留区
    FillRect(
        hdc,
        &band(last.right - SYSTEM_BORDER_BAND, client_width),
        background,
    );

    // 盖干净之后再画回分隔线，保留分段的视觉边界
    FillRect(hdc, &band(0, 1), separator);
    for edge in edges.iter().take(PART_COUNT - 1) {
        FillRect(hdc, &band(*edge, *edge + 1), separator);
    }

    DeleteObject(separator as *mut _);
    DeleteObject(background as *mut _);
    ReleaseDC(window, hdc);
}

/// 按给定宽度算出三段的右边界（最后一段就是宽度本身）。
fn part_edges(width: i32) -> [i32; PART_COUNT] {
    let mut edges = [0i32; PART_COUNT];
    for (slot, percent) in edges.iter_mut().zip(PART_EDGES) {
        *slot = if percent < 0 {
            width
        } else {
            width * percent / 100
        };
    }
    edges
}

/// 分段宽度按状态栏自己的客户区宽度等比例分配。
fn apply_edges(status: HWND) {
    let (width, _) = ui::client_size(status);
    let edges = part_edges(width);
    unsafe { SendMessageW(status, SB_SETPARTS, PART_COUNT, edges.as_ptr() as isize) };
}

/// 一段在状态栏客户区里的实际矩形。
///
/// 不直接用 `DRAWITEMSTRUCT::rcItem`：comctl32 给状态栏留了两处它自己画的边——右端约
/// 一个滚动条宽的非分段区（尺寸柄的位置，实测 27px @150%）和顶部 2px 的立体边，
/// 分段矩形两者都不含。深色主题下这两处会由系统用 `COLOR_BTNFACE` 画成浅色，
/// 而且 `SB_SETBKCOLOR` 管不到它们，于是右下角就留一条白。这里按我们自己算的分段
/// 边界给出整段矩形（含满高），三段拼起来正好铺满整条，那两处浅色就都被盖住了。
fn part_rect(app: &PortManagerApp, index: usize) -> RECT {
    let (width, height) = ui::client_size(app.status);
    let edges = part_edges(width);
    let left = if index == 0 { 0 } else { edges[index - 1] };
    let right = if index + 1 == PART_COUNT {
        width
    } else {
        edges[index]
    };
    RECT {
        left,
        top: 0,
        right,
        bottom: height,
    }
}

/// 状态栏当前高度。
pub(crate) fn height(status: HWND) -> i32 {
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

    for (index, _) in app.status_parts.iter().enumerate() {
        // 自绘分段的文本不经过系统（lparam 是 itemData 而不是字符串指针），
        // 把段号塞进 itemData 带给 WM_DRAWITEM，文字在 draw_item 里现取
        unsafe {
            SendMessageW(
                app.status,
                SB_SETTEXTW,
                index | SBT_OWNERDRAW as usize,
                index as isize,
            )
        };
    }
}

/// 画出一个自绘分段；由主窗口的 `WM_DRAWITEM` 调用。
pub(crate) fn draw_item(app: &PortManagerApp, item: &DRAWITEMSTRUCT) {
    let dc = item.hDC;
    if dc.is_null() {
        return;
    }
    // 段号是我们随 SB_SETTEXTW 一起塞进 itemData 的，比 itemID 更可靠
    let index = item.itemData;
    let Some(text) = app.status_parts.get(index) else {
        return;
    };

    // 浅色借用系统画刷，深色用窗口背景画刷；两个都不归这里释放
    let brush = if app.dark_mode {
        app.background
    } else {
        unsafe { GetSysColorBrush(COLOR_BTNFACE) }
    };

    let mut wide = to_wide(text);
    // 底色铺满是关键：系统只把 rcItem 那块留给我们，右端和顶部它自己留着画浅色，
    // 所以填充用 part_rect（会盖到那两处），文字仍在 rcItem 里排版
    let fill = part_rect(app, index);
    let mut rect = item.rcItem;
    rect.left += ui::scale(TEXT_INSET, app.dpi);
    unsafe {
        FillRect(dc, &fill, brush);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, text_color(app.dark_mode));
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
