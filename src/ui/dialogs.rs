//! 模态对话框：端口详情（§7）与结束进程确认（§6.3）。
//!
//! 两者都是自己注册窗口类的原生模态窗口，而不是 `DialogBoxParam`——这样窗口过程
//! 仍是普通的 Rust 函数，状态通过 `lpParam` 传进去，不必再写 `.rc` 资源脚本。
//!
//! 模态的实现方式是「禁用属主窗口 + 本窗口自己的消息循环」：循环里调用
//! `IsDialogMessageW`，Tab 切换与 Esc 关闭就都由系统负责了。

use std::{fmt::Write as _, ptr};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::{
        HBRUSH, HDC, HFONT, SetBkColor, SetBkMode, SetTextColor, ValidateRect, TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        // EnableWindow / SetFocus / SetActiveWindow 在 windows-sys 里位于输入设备模块
        Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow, SetFocus},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetAncestor,
            GetMessageW, GetWindowLongPtrW, GetWindowRect, IsDialogMessageW, LoadCursorW,
            MoveWindow, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowLongPtrW,
            ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX,
            BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CREATESTRUCTW, ES_AUTOVSCROLL, ES_MULTILINE,
            DLGWINDOWEXTRA, ES_READONLY, GA_ROOT, GWLP_USERDATA, IDC_ARROW, IDCANCEL, IDOK, MSG,
            SW_SHOW, WM_CLOSE,
            WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_CREATE, WM_DESTROY,
            WM_NCCREATE, WM_PAINT, WM_SIZE, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_EX_DLGMODALFRAME,
            WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
        },
    },
};

use crate::{
    app::PortManagerApp,
    models::PortInfo,
    services::{ProcessDetail, ProcessService},
    ui::{self, to_wide},
};

/// 详情对话框里用户做了哪个动作，由调用方执行。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DetailAction {
    /// 直接关掉，什么都没做
    None,
    /// 在资源管理器中定位进程文件
    OpenLocation,
    /// 请求结束该进程（调用方负责再弹一次确认框）
    Kill,
}

/// 只读多行编辑框：有边框与垂直滚动条，长文本自动换行（不使用 ES_AUTOHSCROLL）。
const READONLY_EDIT: u32 = WS_CHILD
    | WS_VISIBLE
    | WS_TABSTOP
    | WS_BORDER
    | WS_VSCROLL
    | ES_MULTILINE as u32
    | ES_READONLY as u32
    | ES_AUTOVSCROLL as u32;
/// 复选框 `BS_AUTOCHECKBOX`；普通按钮与默认按钮。
const CHECKBOX: u32 = WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32;
const PUSHBUTTON: u32 = WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32;
const DEFPUSHBUTTON: u32 = PUSHBUTTON | BS_DEFPUSHBUTTON as u32;
/// `BM_SETCHECK` / `BM_GETCHECK` 用的勾选状态值。
const BST_CHECKED: isize = 1;

// ---------------------------------------------------------------------------
// 模态框架
// ---------------------------------------------------------------------------

/// 注册窗口类；类名已存在时注册会失败，直接忽略即可。
///
/// `cbWndExtra` 必须留出 [`DLGWINDOWEXTRA`] 字节：`IsDialogMessageW` 要靠这段窗口
/// 附加内存保存它自己的对话框状态（默认按钮、Tab 顺序等）。没有这段内存时它会读写
/// 到窗口结构之外，并且对*任意*消息都返回非零——包括 `WM_PAINT`。那样消息循环会把
/// 所有消息都当成「已处理」吞掉，窗口永远不重绘，同时 CPU 一个核跑满。
fn ensure_class(name: &str, procedure: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT) -> Vec<u16> {
    let class_name = to_wide(name);
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let mut window_class: WNDCLASSW = std::mem::zeroed();
        window_class.lpfnWndProc = Some(procedure);
        window_class.hInstance = instance;
        window_class.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
        window_class.cbWndExtra = DLGWINDOWEXTRA as i32;
        window_class.lpszClassName = class_name.as_ptr();
        RegisterClassW(&window_class);
    }
    class_name
}

/// 窗口左上角在屏幕上的位置。
fn window_position(window: HWND) -> (i32, i32) {
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(window, &mut rect) };
    (rect.left, rect.top)
}

/// 模态循环每收到一条消息后该做什么。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pump {
    /// 弹出了 WM_QUIT 或 `GetMessageW` 失败，循环结束
    Stop,
    /// 不是本对话框（含其子控件）的消息，本循环不处理
    Skip,
    /// 本对话框的消息，交给 `IsDialogMessageW` / `TranslateMessage` 处理
    Dispatch,
}

/// 判断一条消息是不是发给 `dialog` 或它的子控件的。
///
/// 用根祖先比较而不是直接比 `hwnd`：编辑框、按钮这些子控件的消息也要算进来，
/// 否则对话框自己就点不动了。
fn pump_of(dialog: HWND, result: i32, message: &MSG) -> Pump {
    if result <= 0 {
        return Pump::Stop;
    }
    if message.hwnd.is_null() {
        // 线程消息（如 WM_QUIT 之外的投递消息）没有窗口，交给下面的默认流程
        return Pump::Dispatch;
    }
    let root = unsafe { GetAncestor(message.hwnd, GA_ROOT) };
    if root == dialog {
        Pump::Dispatch
    } else {
        Pump::Skip
    }
}

/// 放过一条不属于本对话框的消息。
///
/// 大多数消息直接丢掉就是对的——属主窗口此刻是禁用的，本就不该响应输入。
/// 唯独 `WM_PAINT` 不能丢也不能不管：`GetMessageW` 已经把消息从队列里取走，
/// 若既不派发也不清掉无效区域，该区域就一直处于「待重绘」，`GetMessageW` 会立刻
/// 再合成一条 `WM_PAINT`，循环就此空转，一个核直接跑满。
///
/// 这里选择 [`ValidateRect`] 而不是派发：属主窗口重绘需要读 `PortManagerApp`
/// （背景画刷），而调用方正握着它的 `&mut`，派发回去就是别名。直接确认区域为空，
/// 属主保持模态弹出前的画面，等对话框关闭、重新启用时再正常重绘。
unsafe fn skip_message(message: &MSG) {
    if message.message == WM_PAINT && !message.hwnd.is_null() {
        ValidateRect(message.hwnd, ptr::null());
    }
}

/// 创建模态对话框并跑它自己的消息循环，返回携带结果的状态。
///
/// 返回 `None` 表示窗口创建失败。状态在循环结束后才回收，因此窗口过程里可以
/// 放心地把结果写在上面。
///
/// 循环只消化发给本对话框（及其子控件）的消息，其余消息——尤其是发给主窗口的
/// 托盘回调、`WM_COMMAND`——原样留在队列里，等回到主循环再派发。
/// 这一点是必须的：调用方此时正握着 `&mut PortManagerApp`（见 [`show_detail`]、
/// [`confirm_kill`] 的签名），如果这里把主窗口的消息也 `DispatchMessageW` 出去，
/// 主窗口过程就会在借用存续期间再取一份 `&mut`，构成别名 UB。
unsafe fn run_modal<T>(
    owner: HWND,
    class_name: &[u16],
    title: &str,
    state: Box<T>,
    width: i32,
    height: i32,
) -> Option<Box<T>> {
    let instance = GetModuleHandleW(ptr::null());
    let mut title = to_wide(title);
    let parameter = Box::into_raw(state);

    let (owner_width, owner_height) = ui::client_size(owner);
    let (owner_x, owner_y) = window_position(owner);

    let dialog = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        class_name.as_ptr(),
        title.as_mut_ptr(),
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        owner_x + (owner_width - width) / 2,
        owner_y + (owner_height - height) / 2,
        width,
        height,
        owner,
        ptr::null_mut(),
        instance,
        parameter as *const _,
    );
    if dialog.is_null() {
        // 窗口没建成，窗口过程不会跑，这里必须自己回收
        drop(Box::from_raw(parameter));
        return None;
    }

    // 模态：属主被禁用后无法接受输入，用户只能与对话框交互
    EnableWindow(owner, 0);
    ShowWindow(dialog, SW_SHOW);
    SetFocus(dialog);

    let mut message: MSG = std::mem::zeroed();
    loop {
        let result = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
        match pump_of(dialog, result, &message) {
            Pump::Stop => break,
            Pump::Skip => {
                skip_message(&message);
                continue;
            }
            Pump::Dispatch => {}
        }
        // IsDialogMessageW 负责 Tab 切换、Esc 关闭与回车默认按钮。
        //
        // 但 WM_PAINT 绝不能交给它：这条消息被「处理掉」而不派发，意味着
        // BeginPaint / EndPaint 永远不会执行，窗口的无效区域也就永远擦不掉，
        // GetMessageW 会一遍又一遍地重新合成 WM_PAINT——表现为一个核 100% 占用、
        // 界面完全不见刷新。即使窗口类已经补上 DLGWINDOWEXTRA，这个兜底也要留着：
        // 一旦判断失误，代价是整机卡死，而不是少一个 Tab 键。
        if message.message != WM_PAINT && IsDialogMessageW(dialog, &message) != 0 {
            continue;
        }
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }

    EnableWindow(owner, 1);
    SetActiveWindow(owner);
    Some(Box::from_raw(parameter))
}

/// 取出挂在窗口上的状态指针。
unsafe fn state_of<'a, T>(window: HWND) -> Option<&'a mut T> {
    (GetWindowLongPtrW(window, GWLP_USERDATA) as *mut T).as_mut()
}

/// `WM_NCCREATE` 时把 `lpParam` 上的状态挂到窗口上。
unsafe fn attach_state<T>(window: HWND, lparam: LPARAM) -> bool {
    let create = lparam as *const CREATESTRUCTW;
    if create.is_null() {
        return false;
    }
    let state = (*create).lpCreateParams as *mut T;
    if state.is_null() {
        return false;
    }
    SetWindowLongPtrW(window, GWLP_USERDATA, state as isize);
    true
}

/// 创建子控件。
unsafe fn make_child(parent: HWND, class: &str, text: &str, style: u32, id: usize) -> HWND {
    let class = to_wide(class);
    let mut text = to_wide(text);
    CreateWindowExW(
        0,
        class.as_ptr(),
        text.as_mut_ptr(),
        style,
        0,
        0,
        10,
        10,
        parent,
        id as _,
        GetModuleHandleW(ptr::null()),
        ptr::null(),
    )
}

/// 响应 `WM_CTLCOLORSTATIC` / `WM_CTLCOLOREDIT`：设置文字与背景色，并返回背景画刷。
unsafe fn paint_background(dc: HDC, dark: bool, background: HBRUSH) -> LRESULT {
    if !dc.is_null() {
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, if dark { ui::COLOR_DARK_TEXT } else { ui::COLOR_LIGHT_TEXT });
        SetBkColor(dc, if dark { ui::COLOR_DARK_BG } else { ui::COLOR_LIGHT_BG });
    }
    background as isize
}

/// 多行编辑框需要 `\r\n` 才是换行，只有 `\n` 会显示成一个小方块。
fn readonly_text(text: &str) -> String {
    text.replace('\n', "\r\n")
}

// ---------------------------------------------------------------------------
// 端口详情
// ---------------------------------------------------------------------------

/// 详情对话框的控件 ID。
mod detail_ids {
    pub(super) const TEXT: usize = 3001;
    pub(super) const OPEN: usize = 3002;
    pub(super) const KILL: usize = 3003;
}

struct DetailState {
    port: PortInfo,
    action: DetailAction,
    dark: bool,
    background: HBRUSH,
    font: HFONT,
    dpi: u32,
    edit: HWND,
    open: HWND,
    kill: HWND,
    close: HWND,
}

/// 打开端口详情窗口，返回用户选择的动作。
pub(crate) fn show_detail(app: &PortManagerApp, port: &PortInfo) -> DetailAction {
    let class_name = ensure_class("HydraPortDetailDialog", detail_procedure);
    let dpi = app.dpi;
    let state = DetailState {
        port: port.clone(),
        action: DetailAction::None,
        dark: app.dark_mode,
        background: app.background,
        font: app.font,
        dpi,
        edit: ptr::null_mut(),
        open: ptr::null_mut(),
        kill: ptr::null_mut(),
        close: ptr::null_mut(),
    };

    let width = ui::scale(560, dpi);
    let height = ui::scale(460, dpi);
    // SAFETY: 状态是独占的 Box，窗口过程只在消息循环期间借用它
    let state = unsafe {
        run_modal(app.window, &class_name, "端口详情", Box::new(state), width, height)
    };
    state.map_or(DetailAction::None, |state| state.action)
}

unsafe extern "system" fn detail_procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCCREATE => {
            if attach_state::<DetailState>(window, lparam) {
                1
            } else {
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
        WM_CREATE => {
            let Some(state) = state_of::<DetailState>(window) else { return -1 };
            // 详情文本一次性查好写进只读编辑框：既能滚动，也天然支持选中复制
            let text = readonly_text(&detail_text(&state.port));
            state.edit = make_child(window, "Edit", &text, READONLY_EDIT, detail_ids::TEXT);
            state.open = make_child(window, "Button", "打开文件位置", PUSHBUTTON, detail_ids::OPEN);
            state.kill = make_child(window, "Button", "结束进程", PUSHBUTTON, detail_ids::KILL);
            // 关闭按钮用 IDCANCEL，这样 Esc 也能关掉窗口
            state.close = make_child(window, "Button", "关闭", DEFPUSHBUTTON, IDCANCEL as usize);
            ui::apply_font(window, state.font);
            ui::apply_theme(window, state.dark);
            layout_detail(window, state);
            SetFocus(state.edit);
            0
        }
        WM_SIZE => {
            if let Some(state) = state_of::<DetailState>(window) {
                layout_detail(window, state);
            }
            0
        }
        WM_COMMAND => {
            let Some(state) = state_of::<DetailState>(window) else { return 0 };
            match wparam & 0xFFFF {
                id if id == detail_ids::OPEN => {
                    state.action = DetailAction::OpenLocation;
                    DestroyWindow(window);
                }
                id if id == detail_ids::KILL => {
                    state.action = DetailAction::Kill;
                    DestroyWindow(window);
                }
                id if id == IDCANCEL as usize => {
                    DestroyWindow(window);
                }
                _ => {}
            }
            0
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
            let Some(state) = state_of::<DetailState>(window) else { return 0 };
            paint_background(wparam as HDC, state.dark, state.background)
        }
        WM_CLOSE => {
            DestroyWindow(window);
            0
        }
        WM_DESTROY => {
            // 结束自己的消息循环；WM_QUIT 会被内层循环消费掉，不会影响主循环
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// 详情窗口布局：编辑框占满上方，按钮在右下角。
fn layout_detail(window: HWND, state: &DetailState) {
    let (width, height) = ui::client_size(window);
    let margin = ui::scale(10, state.dpi);
    let gap = ui::scale(8, state.dpi);
    let button_height = ui::scale(28, state.dpi);

    let edit_height = (height - margin * 3 - button_height).max(ui::scale(80, state.dpi));
    unsafe {
        MoveWindow(state.edit, margin, margin, (width - margin * 2).max(1), edit_height, 1);

        let y = margin + edit_height + margin;
        let mut right = width - margin;
        // 从右往左摆：关闭 / 结束进程 / 打开文件位置
        for (handle, text) in [(state.close, "关闭"), (state.kill, "结束进程"), (state.open, "打开文件位置")] {
            let button_width = ui::text_width(window, state.font, text) + ui::scale(26, state.dpi);
            right -= button_width;
            MoveWindow(handle, right, y, button_width, button_height, 1);
            right -= gap;
        }
    }
}

/// 端口与进程信息拼成一段文本；查询失败时退回扫描结果并给出原因，避免出现空白区域。
fn detail_text(port: &PortInfo) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "端口：{}", port.port);
    let _ = writeln!(text, "协议：{}", port.protocol);
    let _ = writeln!(text, "本地地址：{}", port.local_address);
    let _ = writeln!(text, "状态：{}", port.state);
    let _ = writeln!(text, "PID：{}", port.pid);

    text.push('\n');
    match ProcessService::get_process_info(port.pid) {
        Ok(detail) => write_process(&mut text, &detail),
        // 系统进程常因权限不足而查不到，这时至少要显示端口扫描拿到的信息
        Err(error) => {
            let _ = writeln!(text, "进程名：{}", display_or_unknown(&port.process_name));
            let _ = writeln!(text, "进程路径：{}", display_or_unknown(&port.process_path));
            text.push('\n');
            let _ = writeln!(text, "无法获取进程详细信息：{error:#}");
        }
    }
    text.trim_end().to_owned()
}

/// 进程信息部分。
fn write_process(text: &mut String, detail: &ProcessDetail) {
    let _ = writeln!(text, "进程名：{}", display_or_unknown(&detail.name));
    let _ = writeln!(
        text,
        "父进程 PID：{}",
        detail.parent_pid.map_or_else(|| "未知".to_owned(), |pid| pid.to_string())
    );
    let _ = writeln!(text, "内存占用：{}", crate::utils::format_bytes(detail.memory_bytes));
    let _ = writeln!(
        text,
        "启动时间：{}",
        detail
            .start_time
            .map_or_else(|| "未知".to_owned(), |time| time.format("%Y-%m-%d %H:%M:%S").to_string())
    );
    let _ = writeln!(text, "命令行：{}", detail.command_line.as_deref().unwrap_or("（无法读取）"));
}

/// 空字符串统一显示为「未知」，避免出现「进程名：」这样的空行。
fn display_or_unknown(value: &str) -> &str {
    if value.is_empty() { "（未知）" } else { value }
}

// ---------------------------------------------------------------------------
// 结束进程确认
// ---------------------------------------------------------------------------

/// 确认对话框的控件 ID。
mod confirm_ids {
    pub(super) const LIST: usize = 3101;
    pub(super) const TREE: usize = 3102;
}

struct ConfirmState {
    /// 提示文本，调用方拼好后带进来
    text: String,
    /// 「同时结束其子进程」的当前值；对话框关闭后由调用方写回 app
    kill_tree: bool,
    /// 用户是否点了「确认结束」
    confirmed: bool,
    dark: bool,
    background: HBRUSH,
    font: HFONT,
    dpi: u32,
    list: HWND,
    tree: HWND,
    ok: HWND,
    cancel: HWND,
}

/// 弹出「确认结束进程」对话框；返回是否确认。
///
/// 同一个进程可能占用多个端口，这里按 PID 去重后再展示，避免用户误判影响范围。
pub(crate) fn confirm_kill(app: &mut PortManagerApp, targets: &[PortInfo]) -> bool {
    let mut pids: Vec<u32> = targets.iter().map(|item| item.pid).collect();
    pids.sort_unstable();
    pids.dedup();

    let mut text = String::new();
    let _ = writeln!(text, "即将结束 {} 个进程：", pids.len());
    for pid in &pids {
        let name = targets
            .iter()
            .find(|item| item.pid == *pid)
            .map_or("未知进程", |item| item.process_name.as_str());
        let _ = writeln!(text, "  · {name}（PID {pid}）");
    }
    text.push_str("\n未保存的数据将会丢失，该操作不可撤销。");

    let class_name = ensure_class("HydraPortConfirmDialog", confirm_procedure);
    let dpi = app.dpi;
    let state = ConfirmState {
        text: text.trim_end().to_owned(),
        kill_tree: app.kill_tree,
        confirmed: false,
        dark: app.dark_mode,
        background: app.background,
        font: app.font,
        dpi,
        list: ptr::null_mut(),
        tree: ptr::null_mut(),
        ok: ptr::null_mut(),
        cancel: ptr::null_mut(),
    };

    let width = ui::scale(440, dpi);
    let height = ui::scale(300, dpi);
    // SAFETY: 状态是独占的 Box，窗口过程只在消息循环期间借用它
    let state = unsafe {
        run_modal(app.window, &class_name, "确认结束进程", Box::new(state), width, height)
    };

    match state {
        Some(state) => {
            app.kill_tree = state.kill_tree;
            state.confirmed
        }
        None => false,
    }
}

unsafe extern "system" fn confirm_procedure(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_NCCREATE => {
            if attach_state::<ConfirmState>(window, lparam) {
                1
            } else {
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
        WM_CREATE => {
            let Some(state) = state_of::<ConfirmState>(window) else { return -1 };
            let text = readonly_text(&state.text);
            // 提示文本不需要参与 Tab 顺序，去掉 WS_TABSTOP
            state.list = make_child(window, "Edit", &text, READONLY_EDIT & !WS_TABSTOP, confirm_ids::LIST);
            state.tree = make_child(window, "Button", "同时结束其子进程", CHECKBOX, confirm_ids::TREE);
            state.ok = make_child(window, "Button", "确认结束", PUSHBUTTON, IDOK as usize);
            state.cancel = make_child(window, "Button", "取消", DEFPUSHBUTTON, IDCANCEL as usize);
            SendMessageW(state.tree, BM_SETCHECK, if state.kill_tree { BST_CHECKED as usize } else { 0 }, 0);
            ui::apply_font(window, state.font);
            ui::apply_theme(window, state.dark);
            layout_confirm(window, state);
            // 焦点默认落在「取消」上，避免顺手一个回车就结束了进程
            SetFocus(state.cancel);
            0
        }
        WM_SIZE => {
            if let Some(state) = state_of::<ConfirmState>(window) {
                layout_confirm(window, state);
            }
            0
        }
        WM_COMMAND => {
            let Some(state) = state_of::<ConfirmState>(window) else { return 0 };
            match wparam & 0xFFFF {
                id if id == confirm_ids::TREE => {
                    state.kill_tree = SendMessageW(state.tree, BM_GETCHECK, 0, 0) == BST_CHECKED;
                }
                id if id == IDOK as usize => {
                    state.confirmed = true;
                    DestroyWindow(window);
                }
                id if id == IDCANCEL as usize => {
                    DestroyWindow(window);
                }
                _ => {}
            }
            0
        }
        // 复选框要一起处理：主题化的按钮文字用的是系统色，深色背景上会看不见
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let Some(state) = state_of::<ConfirmState>(window) else { return 0 };
            paint_background(wparam as HDC, state.dark, state.background)
        }
        WM_CLOSE => {
            DestroyWindow(window);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// 确认窗口布局：提示文本在上，复选框居中偏下，按钮在右下角。
fn layout_confirm(window: HWND, state: &ConfirmState) {
    let (width, height) = ui::client_size(window);
    let margin = ui::scale(10, state.dpi);
    let gap = ui::scale(8, state.dpi);
    let button_height = ui::scale(28, state.dpi);
    let check_height = ui::scale(22, state.dpi);

    unsafe {
        let list_height = (height - margin * 3 - button_height - check_height).max(ui::scale(80, state.dpi));
        MoveWindow(state.list, margin, margin, (width - margin * 2).max(1), list_height, 1);
        MoveWindow(state.tree, margin, margin + list_height + gap, (width - margin * 2).max(1), check_height, 1);

        let y = height - margin - button_height;
        let mut right = width - margin;
        for (handle, text) in [(state.cancel, "取消"), (state.ok, "确认结束")] {
            let button_width = ui::text_width(window, state.font, text) + ui::scale(26, state.dpi);
            right -= button_width;
            MoveWindow(handle, right, y, button_width, button_height, 1);
            right -= gap;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编辑框只认 `\r\n`，转换后不该再残留单独的 `\n`。
    #[test]
    fn readonly_text_uses_crlf() {
        let converted = readonly_text("第一行\n第二行");
        assert_eq!(converted, "第一行\r\n第二行");
        assert!(!converted.replace("\r\n", "").contains('\n'), "仍存在未配对的 \\n");
    }

    /// `GetMessageW` 返回 0（WM_QUIT）或 -1（错误）时都必须结束模态循环，
    /// 否则对话框会卡在那儿等一条永远不会来的消息。
    #[test]
    fn pump_stops_on_quit_and_on_error() {
        let dialog = 1 as HWND;
        let message: MSG = unsafe { std::mem::zeroed() };
        assert_eq!(pump_of(dialog, 0, &message), Pump::Stop);
        assert_eq!(pump_of(dialog, -1, &message), Pump::Stop);
    }

    /// 没有窗口的线程消息（`hwnd` 为空）没有归属，交给正常派发流程。
    #[test]
    fn pump_dispatches_windowless_messages() {
        let mut message: MSG = unsafe { std::mem::zeroed() };
        message.hwnd = ptr::null_mut();
        assert_eq!(pump_of(1 as HWND, 1, &message), Pump::Dispatch);
    }

    /// 详情文本应包含端口与进程的关键字段，且不出现查询失败以外的空白。
    #[test]
    fn detail_text_contains_key_fields() {
        let port = PortInfo {
            port: 8080,
            protocol: "TCP".to_owned(),
            local_address: "0.0.0.0:8080".to_owned(),
            state: "LISTENING".to_owned(),
            pid: std::process::id(),
            process_name: "hydraport.exe".to_owned(),
            process_path: "C:\\test\\hydraport.exe".to_owned(),
        };
        let text = detail_text(&port);
        for expected in ["端口：8080", "协议：TCP", "本地地址：0.0.0.0:8080", "状态：LISTENING", "PID：", "进程名："] {
            assert!(text.contains(expected), "详情文本缺少「{expected}」：\n{text}");
        }
        // 自身进程一定能查到，因此必然带出内存占用一行
        assert!(text.contains("内存占用："), "自身进程的详情应包含内存占用：\n{text}");
    }
}
