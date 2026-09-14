//! 顶部工具栏：搜索、精确查询、刷新、导出、结束选中进程、主题与设置（§7）。
//!
//! 布局按文本实际宽度算出来，所以任何 DPI 下都不会裁字；搜索框吃掉剩余宽度。

use std::ptr;

use windows_sys::Win32::{
    Foundation::HWND,
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::{
            AppendMenuW, CheckMenuItem, CreatePopupMenu, CreateWindowExW, DestroyMenu,
            EnableMenuItem, GetWindowRect, MoveWindow, PostMessageW, SetForegroundWindow,
            TrackPopupMenu, BS_PUSHBUTTON, ES_AUTOHSCROLL, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
            MF_STRING, TPM_RETURNCMD, TPM_RIGHTALIGN, WM_NULL, WS_CHILD, WS_EX_CLIENTEDGE,
            WS_TABSTOP, WS_VISIBLE,
        },
    },
};

use crate::{
    app::{PortManagerApp, ToolbarControls},
    ui::{ids, scale, to_wide},
};

/// 搜索框的最小宽度，窗口再窄也不会把它压没。
const SEARCH_MIN_WIDTH: i32 = 120;
/// 精确端口输入框宽度；至少要放得下「例如 8080」这段提示文字。
const EXACT_WIDTH: i32 = 84;

/// 工具栏上的一个控件。
struct Item {
    id: usize,
    class: &'static str,
    text: &'static str,
    /// 固定宽度；`None` 表示按文本宽度自动计算
    width: Option<i32>,
    /// 是否靠右摆放
    right_aligned: bool,
}

/// 静态标签在 [`PortManagerApp::toolbar`] 的 `labels` 数组里的下标。
const SEARCH_LABEL: usize = 0;
const EXACT_LABEL: usize = 1;

const ITEMS: [Item; 10] = [
    Item {
        id: ids::SEARCH_LABEL,
        class: "Static",
        text: "搜索：",
        width: None,
        right_aligned: false,
    },
    Item {
        id: ids::SEARCH_EDIT,
        class: "Edit",
        text: "",
        width: None,
        right_aligned: false,
    },
    Item {
        id: ids::EXACT_LABEL,
        class: "Static",
        text: "精确端口：",
        width: None,
        right_aligned: false,
    },
    Item {
        id: ids::EXACT_EDIT,
        class: "Edit",
        text: "",
        width: Some(EXACT_WIDTH),
        right_aligned: false,
    },
    Item {
        id: ids::QUERY_BUTTON,
        class: "Button",
        text: "查询",
        width: None,
        right_aligned: false,
    },
    Item {
        id: ids::REFRESH_BUTTON,
        class: "Button",
        text: "刷新",
        width: None,
        right_aligned: false,
    },
    Item {
        id: ids::EXPORT_BUTTON,
        class: "Button",
        text: "导出 CSV",
        width: None,
        right_aligned: false,
    },
    Item {
        id: ids::KILL_BUTTON,
        class: "Button",
        text: "结束选中进程",
        width: None,
        right_aligned: false,
    },
    // 靠右的项按这里的先后顺序从右往左摆，所以「设置」在最前、落在最右角
    Item {
        id: ids::SETTINGS_BUTTON,
        class: "Button",
        text: "设置",
        width: None,
        right_aligned: true,
    },
    Item {
        id: ids::THEME_BUTTON,
        class: "Button",
        text: "浅色主题",
        width: None,
        right_aligned: true,
    },
];

/// 控件的垂直外边距（工具栏高度减去它即为控件高度）。
const VERTICAL_MARGIN: i32 = 5;

/// 工具栏高度。
pub(crate) fn height(dpi: u32) -> i32 {
    scale(34, dpi)
}

/// 创建工具栏上的全部控件。
///
/// 任何一个控件没建起来都算失败：句柄数组会被后面每一轮布局直接拿去 `MoveWindow`，
/// 里面混进空句柄的话，出问题的不是创建这一步，而是若干轮布局之后某个说不清的
/// 现象——那时已经找不到根因了。这里就地报错，附上是哪个控件。
pub(crate) fn create(parent: HWND) -> anyhow::Result<ToolbarControls> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let mut handles = [ptr::null_mut(); ITEMS.len()];

    for (slot, item) in handles.iter_mut().zip(ITEMS.iter()) {
        let class = to_wide(item.class);
        let text = to_wide(item.text);
        // 编辑框靠 WS_EX_CLIENTEDGE 才有可见边框；深色主题下输入区就靠这条边框区分
        let (style, ex_style) = match item.class {
            "Edit" => (
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                WS_EX_CLIENTEDGE,
            ),
            "Button" => (WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32, 0),
            // SS_LEFT == 0，静态控件保持默认左对齐
            _ => (WS_CHILD | WS_VISIBLE, 0),
        };
        *slot = unsafe {
            CreateWindowExW(
                ex_style,
                class.as_ptr(),
                text.as_ptr(),
                style,
                0,
                0,
                10,
                10,
                parent,
                item.id as _,
                instance,
                ptr::null(),
            )
        };
        if slot.is_null() {
            anyhow::bail!(
                "创建工具栏控件「{}」（{}）失败：{}",
                item.text,
                item.class,
                std::io::Error::last_os_error()
            );
        }
    }

    // 搜索框的提示文字（编辑框的 cue banner）
    cue_banner(handles[1], "端口、进程名或 PID");
    cue_banner(handles[3], "例如 8080");

    Ok(ToolbarControls {
        search: handles[1],
        exact: handles[3],
        query: handles[4],
        refresh: handles[5],
        export: handles[6],
        kill: handles[7],
        settings: handles[8],
        theme: handles[9],
        labels: [handles[0], handles[2]],
    })
}

/// 按 [`ITEMS`] 的顺序取出全部句柄；布局只认这一个数组，不依赖子窗口的 Z 序。
fn handles_of(app: &PortManagerApp) -> [HWND; ITEMS.len()] {
    let bar = &app.toolbar;
    [
        bar.labels[SEARCH_LABEL],
        bar.search,
        bar.labels[EXACT_LABEL],
        bar.exact,
        bar.query,
        bar.refresh,
        bar.export,
        bar.kill,
        bar.settings,
        bar.theme,
    ]
}

/// 给编辑框设置灰色的占位提示文字。
fn cue_banner(edit: HWND, text: &str) {
    use windows_sys::Win32::UI::{Controls::EM_SETCUEBANNER, WindowsAndMessaging::SendMessageW};
    let wide = to_wide(text);
    // wParam=TRUE 表示获得焦点后也保留提示
    unsafe { SendMessageW(edit, EM_SETCUEBANNER, 1, wide.as_ptr() as isize) };
}

/// 按客户区宽度重新摆放工具栏控件。
pub(crate) fn layout(app: &PortManagerApp, client_width: i32) {
    let dpi = app.dpi;
    let gap = scale(6, dpi);
    let padding = scale(8, dpi);
    let height = height(dpi);
    let control_height = height - VERTICAL_MARGIN * 2;
    let y = scale(VERTICAL_MARGIN, dpi);

    // 先算固定项的总宽，剩下的全部留给搜索框
    let mut fixed = 0;
    for item in ITEMS.iter().filter(|item| item.id != ids::SEARCH_EDIT) {
        fixed += item_width(app, item, control_height) + gap;
    }
    let search_width = (client_width - fixed - padding * 2).max(scale(SEARCH_MIN_WIDTH, dpi));

    // 靠右的项从右边往左排
    let mut right_edge = client_width - padding;
    let mut right_positions = Vec::new();
    for (index, item) in ITEMS.iter().enumerate() {
        if !item.right_aligned {
            continue;
        }
        let width = item_width(app, item, control_height);
        right_edge -= width;
        right_positions.push((index, right_edge));
        right_edge -= gap;
    }

    let mut x = padding;
    let handles = handles_of(app);
    for (index, item) in ITEMS.iter().enumerate() {
        let handle = handles[index];
        if handle.is_null() {
            continue;
        }
        let (width, left) = if item.id == ids::SEARCH_EDIT {
            (search_width, x)
        } else if let Some((_, left)) = right_positions.iter().find(|(slot, _)| *slot == index) {
            (item_width(app, item, control_height), *left)
        } else {
            (item_width(app, item, control_height), x)
        };

        unsafe { MoveWindow(handle, left, y, width, control_height, 1) };
        if !item.right_aligned {
            x += width + gap;
        }
    }
}

/// 控件宽度：固定宽度优先，否则按文本宽度加内边距。
fn item_width(app: &PortManagerApp, item: &Item, control_height: i32) -> i32 {
    let text = if item.id == ids::THEME_BUTTON {
        theme_button_text(app)
    } else {
        item.text.to_owned()
    };
    match item.width {
        Some(width) => scale(width, app.dpi),
        None => match item.class {
            "Edit" => scale(SEARCH_MIN_WIDTH, app.dpi),
            "Button" => crate::ui::text_width(app.window, app.font, &text) + scale(22, app.dpi),
            // 静态文本只需要文字宽度加一点间隔
            _ => crate::ui::text_width(app.window, app.font, &text) + scale(4, app.dpi),
        },
    }
    .max(control_height)
}

/// 主题按钮的当前文案。
pub(crate) fn theme_button_text(app: &PortManagerApp) -> String {
    if app.dark_mode {
        "浅色主题".to_owned()
    } else {
        "深色主题".to_owned()
    }
}

impl PortManagerApp {
    /// 「结束选中进程」在没选中任何行时应当不可点。
    pub(crate) fn enable_kill_button(&self) {
        unsafe { EnableWindow(self.toolbar.kill, i32::from(!self.selected.is_empty())) };
    }

    /// 主题切换后更新按钮文案。
    pub(crate) fn sync_theme_button(&self) {
        crate::ui::set_control_text(self.toolbar.theme, &theme_button_text(self));
    }
}

/// 「设置」下拉菜单。菜单项一并处理，所以直接返回是否发生了改动。
pub(crate) fn show_settings_menu(app: &mut PortManagerApp) {
    let menu = unsafe { CreatePopupMenu() };
    if menu.is_null() {
        return;
    }

    let tray_available = app.tray_is_active();
    let mut close_label = to_wide("关闭窗口时最小化到托盘");
    let mut autostart_label = to_wide("开机自动启动");
    let copy_label = to_wide("复制日志文件路径");

    unsafe {
        AppendMenuW(
            menu,
            MF_STRING,
            ids::MENU_CLOSE_TO_TRAY,
            close_label.as_mut_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING,
            ids::MENU_AUTOSTART,
            autostart_label.as_mut_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        AppendMenuW(
            menu,
            MF_STRING,
            ids::MENU_COPY_LOG_PATH,
            copy_label.as_ptr(),
        );
    }
    // 勾选状态
    unsafe {
        CheckMenuItem(
            menu,
            ids::MENU_CLOSE_TO_TRAY as u32,
            if app.close_to_tray { MF_CHECKED } else { 0 },
        );
        CheckMenuItem(
            menu,
            ids::MENU_AUTOSTART as u32,
            if app.autostart { MF_CHECKED } else { 0 },
        );
        // 托盘创建失败时该项必须禁用，否则窗口一关就再也打不开
        if !tray_available {
            EnableMenuItem(menu, ids::MENU_CLOSE_TO_TRAY as u32, MF_GRAYED);
        }
    }

    // 菜单贴在「设置」按钮的正下方，右边缘对齐——按钮本身就在窗口右侧，
    // 左对齐会让菜单探出窗口外
    let mut rect = windows_sys::Win32::Foundation::RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetWindowRect(app.toolbar.settings, &mut rect) };
    let command = unsafe {
        SetForegroundWindow(app.window);
        let command = TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_RETURNCMD,
            rect.right,
            rect.bottom,
            0,
            app.window,
            ptr::null(),
        );
        PostMessageW(app.window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        command as usize
    };

    match command {
        ids::MENU_CLOSE_TO_TRAY => {
            app.close_to_tray = !app.close_to_tray;
            app.set_message(if app.close_to_tray {
                "关闭窗口时将最小化到托盘".to_owned()
            } else {
                "关闭窗口时将直接退出".to_owned()
            });
        }
        ids::MENU_AUTOSTART => {
            let enabled = !app.autostart;
            app.set_autostart(enabled);
        }
        ids::MENU_COPY_LOG_PATH => {
            let message = match crate::utils::logger::log_file_path() {
                Some(path) => {
                    let text = path.display().to_string();
                    if crate::utils::clipboard::set_text(app.window, &text) {
                        format!("已复制日志路径：{text}")
                    } else {
                        format!("日志路径：{text}")
                    }
                }
                None => "无法确定日志文件路径".to_owned(),
            };
            app.set_message(message);
        }
        _ => {}
    }
}
