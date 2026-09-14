//! 系统托盘（§1.2 可选项）。
//!
//! 托盘图标需要一个能接收消息的窗口。这里没有去子类化主窗口，而是自己开一个
//! **专用后台线程**，创建不可见的消息窗口并跑独立的消息循环：界面线程即使卡在
//! 一次端口扫描上，托盘菜单依然可用，也不会因为主窗口的销毁顺序而留下「幽灵图标」。
//!
//! 本模块不依赖界面层（§12）：事件以 [`TrayEvent`] 回调出去，由 app 层决定如何唤醒界面。
//! 跨线程只传一个事件编号，界面句柄始终留在界面线程。

use std::{
    cell::{Cell, RefCell},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, LoadImageW,
            PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
            SetForegroundWindow, TrackPopupMenu, TranslateMessage, HMENU, IDI_APPLICATION,
            IMAGE_ICON, LR_DEFAULTCOLOR, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, SM_CYSMICON,
            TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
            WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_POPUP,
        },
    },
};

/// 托盘图标回调使用的自定义消息号。
const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
/// 托盘菜单项的命令 ID。
const MENU_SHOW: usize = 1;
const MENU_REFRESH: usize = 2;
const MENU_EXIT: usize = 3;
/// 托盘窗口的类名（同一进程内唯一即可）。
const WINDOW_CLASS: &str = "HydraPortTrayWindow";
/// 图标资源 ID，与 assets/app.rc 中的 `1 ICON` 对应。
const ICON_RESOURCE_ID: u16 = 1;

/// 托盘菜单触发的动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayEvent {
    /// 显示并激活主窗口
    Show,
    /// 刷新端口列表
    Refresh,
    /// 退出程序
    Exit,
}

impl TrayEvent {
    /// 事件编号。托盘线程与界面线程之间只传这一个整数，避免跨线程搬运数据。
    pub fn code(self) -> usize {
        match self {
            Self::Show => 1,
            Self::Refresh => 2,
            Self::Exit => 3,
        }
    }

    /// 由事件编号还原；未知编号返回 `None`。
    pub fn from_code(code: usize) -> Option<Self> {
        match code {
            1 => Some(Self::Show),
            2 => Some(Self::Refresh),
            3 => Some(Self::Exit),
            _ => None,
        }
    }
}

/// 托盘线程与调用方共享的状态。
struct Shared {
    /// 事件回调，由 app 层提供
    on_event: Box<dyn Fn(TrayEvent) + Send + Sync>,
    /// 托盘窗口句柄，用于退出时投递销毁消息
    window: Mutex<isize>,
    /// 图标是否已经成功添加
    active: AtomicBool,
}

thread_local! {
    /// 托盘线程自己的状态。窗口过程是 C 回调，拿不到 Rust 闭包，只能走线程局部。
    static STATE: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// Explorer 广播的 `TaskbarCreated` 消息号（0 表示尚未注册）
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

/// 托盘服务句柄。析构时会自动移除图标。
pub struct TrayService {
    shared: Arc<Shared>,
}

impl TrayService {
    /// 启动托盘线程并添加图标。
    ///
    /// `on_event` 会在托盘线程上被调用，实现里应尽快返回（通常只是把事件塞进队列并请求
    /// 界面重绘）。返回 `None` 表示托盘创建失败（例如资源管理器未运行）。
    pub fn start(on_event: impl Fn(TrayEvent) + Send + Sync + 'static) -> Option<Self> {
        let shared = Arc::new(Shared {
            on_event: Box::new(on_event),
            window: Mutex::new(0),
            active: AtomicBool::new(false),
        });

        let (ready_sender, ready_receiver) = channel::<bool>();
        let thread_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("hydraport-tray".to_owned())
            .spawn(move || run_message_loop(thread_shared, ready_sender))
            .ok()?;

        // 等待托盘线程完成初始化；超时视为失败，避免界面被卡住
        match ready_receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(true) => Some(Self { shared }),
            _ => None,
        }
    }

    /// 图标是否已经成功添加到通知区域。
    pub fn is_active(&self) -> bool {
        self.shared.active.load(Ordering::Relaxed)
    }

    /// 移除图标并结束托盘线程；重复调用无副作用。
    pub fn shutdown(&self) {
        let window = *self
            .shared
            .window
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if window != 0 {
            // 交给窗口过程移除图标并退出消息循环，避免跨线程直接动窗口
            unsafe { PostMessageW(window as HWND, WM_DESTROY, 0, 0) };
        }
        self.shared.active.store(false, Ordering::Relaxed);
    }
}

impl Drop for TrayService {
    fn drop(&mut self) {
        // 忘记调用 shutdown 时也保证图标被移除，避免通知区域留下「幽灵图标」
        self.shutdown();
    }
}

/// 托盘线程主体：建窗口、加图标、跑消息循环。
fn run_message_loop(shared: Arc<Shared>, ready: Sender<bool>) {
    let added = STATE.with(|slot| {
        *slot.borrow_mut() = Some(Arc::clone(&shared));
        unsafe { create_tray_window(&shared) }
    });
    let _ = ready.send(added);
    if !added {
        // 初始化失败时也要清掉线程局部，避免句柄悬挂
        STATE.with(|slot| *slot.borrow_mut() = None);
        return;
    }

    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) };
        if result <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    STATE.with(|slot| *slot.borrow_mut() = None);
}

/// 在托盘线程上取出共享状态。
fn with_state<R>(action: impl FnOnce(&Shared) -> R) -> Option<R> {
    STATE.with(|slot| slot.borrow().as_ref().map(|shared| action(shared)))
}

/// 创建不可见窗口并添加托盘图标。
unsafe fn create_tray_window(shared: &Shared) -> bool {
    let instance = GetModuleHandleW(ptr::null());
    let class_name = to_wide(WINDOW_CLASS);

    let mut window_class: WNDCLASSW = std::mem::zeroed();
    window_class.lpfnWndProc = Some(window_procedure);
    window_class.hInstance = instance;
    window_class.lpszClassName = class_name.as_ptr();
    // 注册失败通常只是类名已存在（重复创建），不影响后续 CreateWindowExW
    RegisterClassW(&window_class);

    // 用普通的隐藏窗口而不是 HWND_MESSAGE：消息专用窗口无法成为前台窗口，
    // 会导致 TrackPopupMenu 弹出的菜单点击别处时不消失。
    let window = CreateWindowExW(
        0,
        class_name.as_ptr(),
        class_name.as_ptr(),
        WS_POPUP,
        0,
        0,
        0,
        0,
        ptr::null_mut(),
        ptr::null_mut() as HMENU,
        instance,
        ptr::null(),
    );
    if window.is_null() {
        return false;
    }
    *shared
        .window
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = window as isize;

    // Explorer 重启后会广播这个自定义消息，收到后需要重新添加图标
    TASKBAR_CREATED
        .with(|slot| slot.set(RegisterWindowMessageW(to_wide("TaskbarCreated").as_ptr())));

    add_icon(window)
}

/// 向通知区域添加图标。
unsafe fn add_icon(window: HWND) -> bool {
    let mut data: NOTIFYICONDATAW = std::mem::zeroed();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = 1;
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = WM_TRAY_CALLBACK;
    data.hIcon = load_tray_icon();
    let tip = to_wide("端口管理工具");
    let length = tip.len().min(data.szTip.len());
    data.szTip[..length].copy_from_slice(&tip[..length]);

    let added = Shell_NotifyIconW(NIM_ADD, &data) != 0;
    with_state(|shared| shared.active.store(added, Ordering::Relaxed));
    added
}

/// 载入托盘图标：优先使用嵌入到 exe 里的图标（资源 ID 为 1），失败时退回系统默认图标。
unsafe fn load_tray_icon() -> HANDLE {
    let instance = GetModuleHandleW(ptr::null());
    let width = GetSystemMetrics(SM_CXSMICON);
    let height = GetSystemMetrics(SM_CYSMICON);
    // 资源 ID 以指针形式传入（等价于 MAKEINTRESOURCE）
    let icon = LoadImageW(
        instance,
        ICON_RESOURCE_ID as *const u16,
        IMAGE_ICON,
        width,
        height,
        LR_DEFAULTCOLOR,
    );
    if !icon.is_null() {
        return icon;
    }
    LoadImageW(
        ptr::null_mut(),
        IDI_APPLICATION,
        IMAGE_ICON,
        width,
        height,
        LR_DEFAULTCOLOR,
    )
}

/// 窗口过程：处理托盘回调、菜单命令与 Explorer 重启。
unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Explorer 重启后需要重新添加图标
    let taskbar_created = TASKBAR_CREATED.with(|slot| slot.get());
    if taskbar_created != 0 && message == taskbar_created {
        add_icon(window);
        return 0;
    }

    match message {
        // lParam 的低 16 位是鼠标消息
        WM_TRAY_CALLBACK => {
            let event = lparam as u32 & 0xFFFF;
            match event {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(window),
                // 双击显示主窗口；通知区域被折叠进溢出面板时系统可能只发单击，
                // 因此左键抬起同样当作「显示窗口」处理。
                WM_LBUTTONDBLCLK | WM_LBUTTONUP => emit(TrayEvent::Show),
                _ => {}
            }
            log::debug!("托盘回调：鼠标消息 0x{event:04X}");
            0
        }
        WM_COMMAND => {
            match wparam & 0xFFFF {
                MENU_SHOW => emit(TrayEvent::Show),
                MENU_REFRESH => emit(TrayEvent::Refresh),
                MENU_EXIT => emit(TrayEvent::Exit),
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            remove_icon(window);
            // 窗口此时正在被销毁，不能再调用 DestroyWindow
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// 弹出托盘右键菜单。
unsafe fn show_menu(window: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    AppendMenuW(menu, MF_STRING, MENU_SHOW, to_wide("显示主窗口").as_ptr());
    AppendMenuW(
        menu,
        MF_STRING,
        MENU_REFRESH,
        to_wide("刷新端口列表").as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(menu, MF_STRING, MENU_EXIT, to_wide("退出").as_ptr());

    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    // TrackPopupMenu 要求窗口是前台窗口，否则菜单不会在点击别处时消失
    SetForegroundWindow(window);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        cursor.x,
        cursor.y,
        0,
        window,
        ptr::null(),
    );
    // 官方推荐的收尾动作，确保菜单正确关闭
    PostMessageW(window, WM_NULL, 0, 0);
    DestroyMenu(menu);
}

/// 移除托盘图标。
unsafe fn remove_icon(window: HWND) {
    let mut data: NOTIFYICONDATAW = std::mem::zeroed();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = window;
    data.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &data);
}

/// 把事件交给上层回调。
fn emit(event: TrayEvent) {
    with_state(|shared| (shared.on_event)(event));
}

/// 转成以 NUL 结尾的 UTF-16。
fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 托盘服务能正常创建并移除；无人操作时不应产生事件。
    #[test]
    fn tray_starts_and_stops() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let Some(tray) = TrayService::start(move |event| sink.lock().unwrap().push(event)) else {
            // 无桌面会话（例如服务方式运行）时托盘不可用，跳过
            return;
        };
        assert!(tray.is_active(), "托盘图标应已添加");
        std::thread::sleep(Duration::from_millis(100));
        assert!(events.lock().unwrap().is_empty(), "未操作托盘时不应有事件");
        tray.shutdown();
    }

    /// 重复调用 shutdown 不应 panic（Drop 里还会再调一次）。
    #[test]
    fn shutdown_is_idempotent() {
        if let Some(tray) = TrayService::start(|_| {}) {
            tray.shutdown();
            tray.shutdown();
        }
    }

    /// 事件编号与还原必须一一对应，且未知编号不应被误认。
    #[test]
    fn event_codes_round_trip() {
        for event in [TrayEvent::Show, TrayEvent::Refresh, TrayEvent::Exit] {
            assert_eq!(TrayEvent::from_code(event.code()), Some(event));
        }
        assert_eq!(TrayEvent::from_code(0), None);
        assert_eq!(TrayEvent::from_code(99), None);
    }

    /// 宽字符串编码应以 NUL 结尾。
    #[test]
    fn wide_string_is_nul_terminated() {
        let wide = to_wide("托盘");
        assert_eq!(wide.len(), 3);
        assert_eq!(wide[2], 0);
    }
}
