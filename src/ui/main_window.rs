//! 主窗口：窗口类注册、消息循环与全部消息分发（§3、§8）。
//!
//! 窗口过程只做「取状态 → 交给 app / 各子模块」这一件事：真正的业务在
//! [`crate::app`]，控件的创建与布局在 toolbar / table / status_bar 里。

use std::{cell::Cell, collections::BTreeSet, ptr};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        FillRect, InvalidateRect, RedrawWindow, ScreenToClient, SetBkColor, SetBkMode,
        SetTextColor, UpdateWindow, COLOR_WINDOW, HBRUSH, HDC, RDW_ALLCHILDREN, RDW_ERASE,
        RDW_INVALIDATE, TRANSPARENT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            InitCommonControlsEx, DRAWITEMSTRUCT, ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES,
            ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, LVN_COLUMNCLICK, LVN_ITEMCHANGED, NMHDR,
            NMLISTVIEW, NM_CUSTOMDRAW, NM_DBLCLK, NM_RCLICK,
        },
        HiDpi::AdjustWindowRectExForDpi,
        Input::KeyboardAndMouse::VK_F5,
        WindowsAndMessaging::{
            AdjustWindowRectEx, CreateAcceleratorTableW, CreateWindowExW, DefWindowProcW,
            DestroyAcceleratorTable, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos,
            GetMessageW, GetWindowLongPtrW, IsIconic, LoadCursorW, LoadImageW, MoveWindow,
            PostQuitMessage, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
            SetWindowPos, ShowWindow, TranslateAcceleratorW, TranslateMessage, ACCEL, CS_HREDRAW,
            CS_VREDRAW, CW_USEDEFAULT, EN_CHANGE, FVIRTKEY, GWLP_USERDATA, IDC_ARROW, IMAGE_ICON,
            LR_DEFAULTSIZE, LR_SHARED, MINMAXINFO, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
            SW_HIDE, SW_RESTORE, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
            WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND,
            WM_GETMINMAXINFO, WM_NCDESTROY, WM_NOTIFY, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        },
    },
};

use crate::{
    app::PortManagerApp,
    services::{PortQuery, TrayEvent},
    ui::{self, dialogs::DetailAction, ids, scale, to_wide, WM_TRAY_EVENT},
};

/// 窗口类名与标题。
const CLASS_NAME: &str = "HydraPortMainWindow";
const WINDOW_TITLE: &str = "HydraPort 端口管理工具";
/// 图标资源 ID，与 `assets/app.rc` 中的 `1 ICON` 对应。
const ICON_RESOURCE_ID: u16 = 1;
/// 初始客户区尺寸（96 DPI 下的逻辑像素）。
const INITIAL_SIZE: (i32, i32) = (1100, 700);
/// 最小客户区尺寸，避免窗口被压得放不下工具栏。
const MIN_SIZE: (i32, i32) = (720, 420);

/// 全局加速键：F5 = 刷新。
const ACCELERATORS: [ACCEL; 1] = [ACCEL {
    fVirt: FVIRTKEY,
    key: VK_F5,
    cmd: ids::REFRESH_BUTTON as u16,
}];

// 消息派发本身就是重入的：一个窗口过程里跑模态对话框，对话框的消息循环又能
// 派发到本窗口；`DispatchMessageW` 期间控件也可能回调进来。只要有第二次取用，
// 两个 `&mut` 就同时活着，属于别名 UB——而且往往表现为难以复现的状态错乱。
//
// 用线程局部计数把它变成一次明确的中止：重入的那一层直接走 `DefWindowProcW`，
// 消息不丢（走默认处理），也不会生成第二个 `&mut`。
// 计数而不是布尔，是为了让「本层的 `app_mut` 何时归还」这件事保持成对。
thread_local! {
    static APP_BORROWS: Cell<u32> = const { Cell::new(0) };
}

/// 借用守卫：`Drop` 时把计数还回去。
struct AppBorrow;

impl Drop for AppBorrow {
    fn drop(&mut self) {
        APP_BORROWS.with(|count| count.set(count.get().saturating_sub(1)));
    }
}

/// 取回挂在窗口上的应用状态。
fn app_ref<'a>(window: HWND) -> Option<&'a PortManagerApp> {
    unsafe { (GetWindowLongPtrW(window, GWLP_USERDATA) as *const PortManagerApp).as_ref() }
}

/// 同上，但要可变借用；调用期间不得再取第二份。
///
/// 重入时返回 `None`，调用方应当退回 `DefWindowProcW` 而不是继续处理。
fn app_mut<'a>(window: HWND) -> Option<(&'a mut PortManagerApp, AppBorrow)> {
    if APP_BORROWS.with(|count| count.get()) > 0 {
        log::debug!("检测到窗口过程重入，本次消息按默认处理");
        return None;
    }
    let app =
        unsafe { (GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PortManagerApp).as_mut() }?;
    APP_BORROWS.with(|count| count.set(count.get() + 1));
    Some((app, AppBorrow))
}

/// 加载通用控件库，注册列表视图、表头、状态栏等窗口类。
///
/// 清单里已经声明了 comctl32 v6，多数情况下类也会在进程启动时注册好；但
/// `InitCommonControlsEx` 是文档要求的调用，少了它，「类是否已注册」就变成
/// 依赖 comctl32 的加载时机——现在是能跑通的，但那是别的调用顺手把它拉进来了，
/// 一旦哪天那些调用被挪走或去掉，`CreateWindowExW` 就会直接返回 NULL。
///
/// `ICC_LISTVIEW_CLASSES` 覆盖 `SysListView32` 与 `SysHeader32`，
/// `ICC_BAR_CLASSES` 覆盖状态栏，`ICC_STANDARD_CLASSES` 覆盖按钮与编辑框。
fn init_common_controls() {
    let classes = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES | ICC_STANDARD_CLASSES,
    };
    // 返回 FALSE 时不必中断启动：清单声明的 v6 仍然会把类注册好，
    // 真正的判据是下面每个控件能否创建成功
    if unsafe { InitCommonControlsEx(&classes) } == 0 {
        log::warn!("InitCommonControlsEx 返回失败，改为依赖清单注册的控件类");
    }
}

/// 创建主窗口并跑消息循环，直到用户退出。
pub(crate) fn run() -> anyhow::Result<()> {
    // 先注册控件类，再切换深色模式；两者都必须在创建任何控件之前完成
    init_common_controls();
    // 必须在创建任何控件之前切换，否则列表与状态栏不会跟随深色主题
    ui::dark_mode::enable_for_process();

    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let class_name = to_wide(CLASS_NAME);

        let mut window_class: WNDCLASSW = std::mem::zeroed();
        window_class.style = CS_HREDRAW | CS_VREDRAW;
        window_class.lpfnWndProc = Some(window_procedure);
        window_class.hInstance = instance;
        window_class.hIcon = LoadImageW(
            instance,
            ICON_RESOURCE_ID as *const u16,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        );
        window_class.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
        window_class.hbrBackground = (COLOR_WINDOW + 1) as HBRUSH;
        window_class.lpszClassName = class_name.as_ptr();
        if RegisterClassW(&window_class) == 0 {
            anyhow::bail!("注册主窗口类失败");
        }

        // 先按 96 DPI 的逻辑尺寸建窗，拿到窗口句柄后立刻按实际 DPI 调整
        let style = WS_OVERLAPPEDWINDOW;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: INITIAL_SIZE.0,
            bottom: INITIAL_SIZE.1,
        };
        AdjustWindowRectEx(&mut frame, style, 0, 0);

        let title = to_wide(WINDOW_TITLE);
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            frame.right - frame.left,
            frame.bottom - frame.top,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        if window.is_null() {
            anyhow::bail!("创建主窗口失败");
        }

        // 控件必须在窗口存在之后创建；这之前的消息由窗口过程按「无状态」处理
        let app = match PortManagerApp::new(window) {
            Ok(app) => Box::new(app),
            Err(error) => {
                // 窗口已经建出来了，失败路径上得自己收尾：销毁它会走 WM_NCDESTROY，
                // 那时 GWLP_USERDATA 还是空，窗口过程不会去动尚未建立的状态
                DestroyWindow(window);
                return Err(error);
            }
        };
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(app) as isize);

        if let Some(app) = app_ref(window) {
            let dpi = app.dpi;
            let mut frame = RECT {
                left: 0,
                top: 0,
                right: scale(INITIAL_SIZE.0, dpi),
                bottom: scale(INITIAL_SIZE.1, dpi),
            };
            AdjustWindowRectExForDpi(&mut frame, style, 0, 0, dpi);
            SetWindowPos(
                window,
                ptr::null_mut(),
                0,
                0,
                frame.right - frame.left,
                frame.bottom - frame.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            layout(app);
        }

        ShowWindow(window, SW_SHOW);
        UpdateWindow(window);
        log::info!(
            "主窗口已显示，启动耗时 {} ms",
            crate::utils::timing::elapsed_ms()
        );

        let accelerators =
            CreateAcceleratorTableW(ACCELERATORS.as_ptr(), ACCELERATORS.len() as i32);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {
            // 加速键要先于控件处理，否则 F5 会被当前获得焦点的控件吃掉。
            //
            // 这里显式保留 `&mut`：真身的 `TranslateAcceleratorW` 收的是 `LPMSG`（可写），
            // 命中加速键时它会把消息就地改写成 WM_COMMAND。windows-sys 把这个参数标成了
            // `*const MSG`，clippy 便认为不必可变——按它的建议改成 `&` 会把「可能被写」
            // 这件事藏起来，因此这里按真实语义传可变引用，只放行这一条 lint。
            #[allow(clippy::unnecessary_mut_passed)]
            let translated = !accelerators.is_null()
                && TranslateAcceleratorW(window, accelerators, &mut message) != 0;
            if translated {
                continue;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if !accelerators.is_null() {
            DestroyAcceleratorTable(accelerators);
        }
    }
    Ok(())
}

/// 按客户区尺寸重排工具栏、表格与状态栏。
fn layout(app: &PortManagerApp) {
    let (width, height) = ui::client_size(app.window);
    if width <= 0 || height <= 0 {
        return;
    }

    // 状态栏自己会贴着父窗口底边重排，先让它处理 WM_SIZE 再问它高度
    unsafe { SendMessageW(app.status, WM_SIZE, 0, 0) };
    ui::status_bar::resize(app);
    let status_height = ui::status_bar::height(app.status);

    let toolbar_height = ui::toolbar::height(app.dpi);
    ui::toolbar::layout(app, width);

    let list_height = (height - toolbar_height - status_height).max(1);
    unsafe { MoveWindow(app.list, 0, toolbar_height, width, list_height, 1) };
    // 表格宽度刚刚才定下来，最后一列要按新宽度重新拉满；列表视图自己不会做这件事
    app.stretch_last_column();
}

impl PortManagerApp {
    /// 刷新窗口标题。非管理员时把身份写进标题：结束系统进程会失败，
    /// 用户需要一眼看到原因（§1.1），标题在任务栏里也一直可见。
    pub(crate) fn sync_window_title(&self) {
        let title = if self.administrator {
            WINDOW_TITLE.to_owned()
        } else {
            format!("{WINDOW_TITLE}（非管理员）")
        };
        ui::set_control_text(self.window, &title);
    }
}

/// 显示并激活主窗口（托盘图标点击 / 托盘菜单）。
pub(crate) fn show(app: &PortManagerApp) {
    unsafe {
        if IsIconic(app.window) != 0 {
            ShowWindow(app.window, SW_RESTORE);
        } else {
            ShowWindow(app.window, SW_SHOW);
        }
        SetForegroundWindow(app.window);
    }
}

/// 隐藏主窗口（最小化到托盘）。
pub(crate) fn hide(app: &PortManagerApp) {
    unsafe { ShowWindow(app.window, SW_HIDE) };
}

/// 打开当前选中行的详情窗口，并执行用户在窗口里选择的动作。
fn show_detail(app: &mut PortManagerApp) {
    let Some(port) = app.context_port() else {
        return;
    };
    match ui::dialogs::show_detail(app, &port) {
        DetailAction::None => {}
        DetailAction::OpenLocation => app.open_file_location(&port.process_path),
        DetailAction::Kill => {
            if ui::dialogs::confirm_kill(app, std::slice::from_ref(&port)) {
                app.kill_ports(std::slice::from_ref(&port));
            } else {
                app.set_message("已取消结束进程".to_owned());
            }
        }
    }
}

/// 结束当前选中的全部进程（先确认）。
fn kill_selection(app: &mut PortManagerApp) {
    let ports = app.selected_ports();
    if ports.is_empty() {
        app.set_message("请先选择要结束的进程".to_owned());
        return;
    }
    if ui::dialogs::confirm_kill(app, &ports) {
        app.kill_ports(&ports);
    } else {
        app.set_message("已取消结束进程".to_owned());
    }
}

/// 复制一段文本到剪贴板，并把结果写进状态栏。
fn copy_text(app: &mut PortManagerApp, label: &str, text: &str) {
    let message = if crate::utils::clipboard::set_text(app.window, text) {
        format!("已复制{label}：{text}")
    } else {
        format!("剪贴板不可用，{label}：{text}")
    };
    app.set_message(message);
}

/// 切换深色 / 浅色主题；画刷与控件外观都要跟着换。
fn toggle_theme(app: &mut PortManagerApp) {
    app.dark_mode = !app.dark_mode;
    let previous = app.background;
    app.background = ui::create_background_brush(app.dark_mode);
    ui::apply_theme(app.window, app.dark_mode);
    app.sync_theme_button();
    // 新画刷已经生效，旧画刷这时才可以删；窗口与子控件都需要重画一遍
    ui::delete_object(previous as *mut _);
    unsafe {
        InvalidateRect(app.window, ptr::null(), 1);
        RedrawWindow(
            app.window,
            ptr::null(),
            ptr::null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
    }
    app.set_message(if app.dark_mode {
        "已切换到深色主题".to_owned()
    } else {
        "已切换到浅色主题".to_owned()
    });
}

/// 「查询」按钮的提示语：填了精确端口时额外说明有多少个进程在占用它。
fn query_message(app: &PortManagerApp) -> String {
    match app.exact_port.trim().parse::<u16>().ok() {
        Some(value) => {
            let hits = PortQuery::find_by_port(&app.ports, value);
            if hits.is_empty() {
                format!("没有进程监听端口 {value}")
            } else {
                // 同一端口可能被多个协议或多个进程同时占用，去重后才好读
                let pids: BTreeSet<u32> = hits.iter().map(|item| item.pid).collect();
                format!(
                    "端口 {value} 命中 {} 条记录，来自 {} 个进程",
                    hits.len(),
                    pids.len()
                )
            }
        }
        None => format!("查询完成，显示 {} 条", app.rows.len()),
    }
}

/// 处理表格右键菜单的命令（菜单由 `table::show_table_menu` 弹出）。
fn handle_table_menu(app: &mut PortManagerApp, command: usize) {
    let Some(port) = app.context_port() else {
        return;
    };
    match command {
        ids::MENU_COPY_PORT => copy_text(app, "端口", &port.port.to_string()),
        ids::MENU_COPY_PID => copy_text(app, "PID", &port.pid.to_string()),
        ids::MENU_DETAIL => show_detail(app),
        ids::MENU_OPEN_LOCATION => app.open_file_location(&port.process_path),
        ids::MENU_KILL => {
            if ui::dialogs::confirm_kill(app, std::slice::from_ref(&port)) {
                app.kill_ports(std::slice::from_ref(&port));
            } else {
                app.set_message("已取消结束进程".to_owned());
            }
        }
        _ => {}
    }
}

/// 处理 `WM_COMMAND`：工具栏按钮、编辑框通知与右键菜单命令都走这里。
fn handle_command(app: &mut PortManagerApp, wparam: WPARAM) {
    let id = wparam & 0xFFFF;
    let notification = (wparam >> 16) as u32;

    match id {
        id if id == ids::SEARCH_EDIT || id == ids::EXACT_EDIT => {
            if notification != EN_CHANGE {
                return;
            }
            // 输入即筛选：编辑框内容直接决定过滤条件
            app.search = ui::control_text(app.toolbar.search);
            app.exact_port = ui::control_text(app.toolbar.exact);
            app.mark_rows_dirty();
            app.rebuild_rows();
        }
        id if id == ids::QUERY_BUTTON => {
            app.search = ui::control_text(app.toolbar.search);
            app.exact_port = ui::control_text(app.toolbar.exact);
            app.mark_rows_dirty();
            app.rebuild_rows();
            let message = query_message(app);
            app.set_message(message);
        }
        id if id == ids::REFRESH_BUTTON => app.refresh(),
        id if id == ids::EXPORT_BUTTON => app.export_csv(),
        id if id == ids::KILL_BUTTON => kill_selection(app),
        id if id == ids::THEME_BUTTON => toggle_theme(app),
        id if id == ids::SETTINGS_BUTTON => ui::toolbar::show_settings_menu(app),
        ids::MENU_COPY_PORT
        | ids::MENU_COPY_PID
        | ids::MENU_DETAIL
        | ids::MENU_OPEN_LOCATION
        | ids::MENU_KILL => handle_table_menu(app, id),
        _ => {}
    }
}

/// 处理 `WM_NOTIFY`：关心列表视图发来的通知，以及它和表头的自绘请求。
///
/// 返回 `LRESULT`：自绘通知要把 `CDRF_*` 回给控件，其余一律 0。
fn handle_notify(app: &mut PortManagerApp, lparam: LPARAM) -> LRESULT {
    let header = lparam as *const NMHDR;
    if header.is_null() {
        return 0;
    }
    // 通知码在 windows-sys 里可能是 u32 也可能是 i32，统一按 i32 比较
    let code = unsafe { (*header).code } as i32;
    let from = unsafe { (*header).hwndFrom };
    let id = unsafe { (*header).idFrom };

    // 自绘通知只从列表视图来：表头（SysHeader32）根本不发 NM_CUSTOMDRAW，
    // 它的深色由 table::install_header_subclass 换掉窗口过程单独处理。
    if code == NM_CUSTOMDRAW as i32 && from == app.list {
        return super::table::handle_custom_draw(app, lparam);
    }

    if id != ids::LIST_VIEW {
        return 0;
    }

    if code == LVN_ITEMCHANGED as i32 {
        app.sync_selection_from_table();
    } else if code == LVN_COLUMNCLICK as i32 {
        let clicked = unsafe { (lparam as *const NMLISTVIEW).as_ref() };
        if let Some(column) = clicked.and_then(|item| super::table::sort_column_of(item.iSubItem)) {
            app.toggle_sort(column);
            app.rebuild_rows();
        }
    } else if code == NM_DBLCLK as i32 {
        show_detail(app);
    } else if code == NM_RCLICK as i32 {
        // 菜单按屏幕坐标弹出，命中测试需要客户区坐标
        let mut cursor = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut cursor);
            ScreenToClient(app.list, &mut cursor);
        }
        app.show_table_menu(cursor);
    }

    0
}

/// 主窗口过程。
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // 销毁时必须先把状态的所有权取回来，之后才能调用 &mut 方法
    if message == WM_NCDESTROY {
        let pointer = SetWindowLongPtrW(window, GWLP_USERDATA, 0) as *mut PortManagerApp;
        if !pointer.is_null() {
            let mut app = Box::from_raw(pointer);
            app.shutdown();
            drop(app);
        }
        return DefWindowProcW(window, message, wparam, lparam);
    }

    // 窗口创建期间 GWLP_USERDATA 尚未写入，这段消息交给系统默认处理；
    // 重入的情况同样在这里挡住，见 app_mut 的说明
    let Some((app, _borrow)) = app_mut(window) else {
        return DefWindowProcW(window, message, wparam, lparam);
    };

    match message {
        WM_SIZE => {
            layout(app);
            0
        }
        WM_GETMINMAXINFO => {
            let info = lparam as *mut MINMAXINFO;
            if !info.is_null() {
                (*info).ptMinTrackSize = POINT {
                    x: scale(MIN_SIZE.0, app.dpi),
                    y: scale(MIN_SIZE.1, app.dpi),
                };
            }
            0
        }
        WM_DPICHANGED => {
            let dpi = (wparam & 0xFFFF) as u32;
            let suggested = lparam as *const RECT;
            if !suggested.is_null() {
                let rect = *suggested;
                SetWindowPos(
                    window,
                    ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            app.dpi = dpi;
            let previous = app.font;
            app.font = ui::create_ui_font(dpi);
            ui::apply_font(window, app.font);
            layout(app);
            // 布局算完才能删旧字体：text_width 用的就是它
            ui::delete_object(previous as *mut _);
            0
        }
        WM_COMMAND => {
            handle_command(app, wparam);
            0
        }
        WM_NOTIFY => handle_notify(app, lparam),
        WM_TRAY_EVENT => {
            if let Some(event) = TrayEvent::from_code(wparam) {
                app.handle_tray_event(event);
                if app.quit_requested {
                    DestroyWindow(window);
                }
            }
            0
        }
        WM_DRAWITEM => {
            // 深色主题下状态栏分段是自绘的，这里是唯一能给它上色的地方
            let item = lparam as *const DRAWITEMSTRUCT;
            if let Some(item) = item.as_ref() {
                if item.hwndItem == app.status {
                    ui::status_bar::draw_item(app, item);
                }
            }
            1
        }
        WM_ERASEBKGND => {
            // 背景自己填，窗口类里的系统画刷不随主题变化
            let dc = wparam as HDC;
            let mut rect: RECT = std::mem::zeroed();
            GetClientRect(window, &mut rect);
            FillRect(dc, &rect, app.background);
            1
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            // 工具栏上的静态标签与编辑框都要按当前主题着色
            let dc = wparam as HDC;
            if !dc.is_null() {
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(
                    dc,
                    if app.dark_mode {
                        ui::COLOR_DARK_TEXT
                    } else {
                        ui::COLOR_LIGHT_TEXT
                    },
                );
                SetBkColor(
                    dc,
                    if app.dark_mode {
                        ui::COLOR_DARK_BG
                    } else {
                        ui::COLOR_LIGHT_BG
                    },
                );
            }
            app.background as isize
        }
        WM_CLOSE => {
            app.handle_close_request();
            // 只是最小化到托盘时窗口要留着，否则托盘图标就再也点不开了
            if app.quit_requested {
                DestroyWindow(window);
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
