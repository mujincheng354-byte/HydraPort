//! 原生 Win32 参照程序：一个窗口 + report 模式 ListView（7 列 / 300 行）。
//!
//! 用来测定「如果按开发文档 §2 的备选方案（winsafe / native-windows-gui 这一类
//! 纯 Win32 控件）重写界面层」时的内存下限——不创建 OpenGL 上下文，因而
//! 不会把显卡驱动拉进进程。

use std::{mem::zeroed, ptr};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    Graphics::Gdi::COLOR_WINDOW,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            InitCommonControlsEx, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LVCF_SUBITEM,
            LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
            LVM_SETITEMTEXTW, LVS_REPORT, WC_LISTVIEWW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
            MoveWindow, PostQuitMessage, RegisterClassW, SendMessageW, TranslateMessage,
            CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG, WNDCLASSW, WM_DESTROY, WS_CHILD,
            WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        },
    },
};

const COLUMNS: [&str; 7] = ["端口", "协议", "本地地址", "状态", "PID", "进程名", "进程路径"];
const ROWS: usize = 300;

const LIST_ID: usize = 1001;

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let class_name = wide("HydraPortBenchWindow");

        let mut controls: INITCOMMONCONTROLSEX = zeroed();
        controls.dwSize = std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32;
        controls.dwICC = ICC_LISTVIEW_CLASSES;
        InitCommonControlsEx(&controls);

        let mut window_class: WNDCLASSW = zeroed();
        window_class.style = CS_HREDRAW | CS_VREDRAW;
        window_class.lpfnWndProc = Some(window_procedure);
        window_class.hInstance = instance;
        window_class.hbrBackground = (COLOR_WINDOW + 1) as _;
        window_class.lpszClassName = class_name.as_ptr();
        RegisterClassW(&window_class);

        let title = wide("win32-listview");
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1100,
            700,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );

        let list = CreateWindowExW(
            0,
            WC_LISTVIEWW,
            ptr::null(),
            WS_CHILD | WS_VISIBLE | LVS_REPORT,
            0,
            0,
            0,
            0,
            window,
            LIST_ID as _,
            instance,
            ptr::null(),
        );
        fill_list(list);

        let mut client = zeroed();
        GetClientRect(window, &mut client);
        MoveWindow(list, 0, 0, client.right, client.bottom, 1);

        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// 插入表头与若干行，模拟真实端口列表的体量。
unsafe fn fill_list(list: HWND) {
    for (index, name) in COLUMNS.iter().enumerate() {
        let mut text = wide(name);
        let mut column: LVCOLUMNW = zeroed();
        column.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM;
        column.cx = 150;
        column.iSubItem = index as i32;
        column.pszText = text.as_mut_ptr();
        SendMessageW(list, LVM_INSERTCOLUMNW, index, &column as *const _ as isize);
    }

    for row in 0..ROWS {
        let first = format!("{}", 3000 + row);
        let mut first_text = wide(&first);
        let mut item: LVITEMW = zeroed();
        item.mask = LVIF_TEXT;
        item.iItem = row as i32;
        item.pszText = first_text.as_mut_ptr();
        SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as isize);

        for (index, value) in [
            "TCP",
            "0.0.0.0",
            "LISTENING",
            &format!("{}", 1000 + row),
            "example.exe",
            "C:\\Program Files\\Example\\example.exe",
        ]
        .iter()
        .enumerate()
        {
            let mut text = wide(value);
            let mut sub: LVITEMW = zeroed();
            sub.iSubItem = index as i32 + 1;
            sub.pszText = text.as_mut_ptr();
            SendMessageW(list, LVM_SETITEMTEXTW, row, &sub as *const _ as isize);
        }
    }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
