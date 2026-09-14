//! 中央端口表格：原生列表视图（§7）。
//!
//! 多选（Ctrl 追加、Shift 区间）由列表视图自己实现，不需要手写；
//! 这里只负责填数据、排序箭头、选中同步与右键菜单。

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, InvalidateRect, HFONT, TRANSPARENT},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            HDF_SORTDOWN, HDF_SORTUP, HDITEMW, HDI_FORMAT, HDI_TEXT, HDM_GETITEMCOUNT,
            HDM_GETITEMRECT, HDM_GETITEMW, HDM_SETITEMW, LVCFMT_LEFT, LVCFMT_RIGHT, LVCF_FMT,
            LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVHITTESTINFO, LVHT_ONITEMICON,
            LVHT_ONITEMLABEL, LVHT_ONITEMSTATEICON, LVIF_STATE, LVIF_TEXT, LVIS_SELECTED, LVITEMW,
            LVM_DELETEALLITEMS, LVM_GETCOLUMNWIDTH, LVM_GETHEADER, LVM_GETNEXTITEM,
            LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE,
            LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVM_SUBITEMHITTEST, LVNI_SELECTED,
            LVSCW_AUTOSIZE_USEHEADER, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_GRIDLINES,
            LVS_REPORT, LVS_SHOWSELALWAYS, WC_LISTVIEWW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, GetClientRect, SendMessageW, WM_GETFONT, WS_CHILD, WS_TABSTOP,
            WS_VISIBLE,
        },
    },
};

/// 点在行的有效区域上（图标、文字或状态图标）才算命中该行。
const LVHT_ONITEM: u32 = LVHT_ONITEMICON | LVHT_ONITEMLABEL | LVHT_ONITEMSTATEICON;

use crate::{
    app::{key_of, PortManagerApp, SortColumn},
    ui::{ids, to_wide},
};

/// 表格列：标题、对齐方式、是否可点表头排序。
const COLUMNS: [(&str, i32, bool); 7] = [
    ("端口", LVCFMT_RIGHT, true),
    ("协议", LVCFMT_LEFT, true),
    ("本地地址", LVCFMT_LEFT, false),
    ("状态", LVCFMT_LEFT, false),
    ("PID", LVCFMT_RIGHT, true),
    ("进程名", LVCFMT_LEFT, true),
    ("进程路径", LVCFMT_LEFT, true),
];

/// 创建列表视图并插入表头。
///
/// 列表视图是整个界面里唯一不能缺的控件：没有它就没有端口可看。因此这里
/// 一旦创建失败就返回错误，让启动直接失败得明明白白，而不是留一个空窗口。
pub(crate) fn create(parent: HWND) -> anyhow::Result<HWND> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    let list = unsafe {
        CreateWindowExW(
            0,
            WC_LISTVIEWW,
            ptr::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | LVS_REPORT | LVS_SHOWSELALWAYS,
            0,
            0,
            0,
            0,
            parent,
            ids::LIST_VIEW as _,
            instance,
            ptr::null(),
        )
    };
    if list.is_null() {
        anyhow::bail!("创建列表视图失败：{}", std::io::Error::last_os_error());
    }

    // 整行选中 + 双缓冲（避免刷新时闪烁）+ 网格线
    unsafe {
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_GRIDLINES) as isize,
        )
    };

    for (index, (title, format, _)) in COLUMNS.iter().enumerate() {
        let mut text = to_wide(title);
        let mut column: LVCOLUMNW = unsafe { std::mem::zeroed() };
        column.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM | LVCF_FMT;
        column.fmt = *format;
        column.cx = 120;
        column.iSubItem = index as i32;
        column.pszText = text.as_mut_ptr();
        // 插列失败只影响这一列（表头会少一格），不值得让整个程序起不来
        let inserted =
            unsafe { SendMessageW(list, LVM_INSERTCOLUMNW, index, &column as *const _ as isize) };
        if inserted == 0 {
            log::warn!("插入表格列「{title}」失败");
        }
    }

    Ok(list)
}

impl PortManagerApp {
    /// 用当前 `rows` 重新填充表格，并尽量保留原有选中项。
    pub(crate) fn reload_table(&mut self) {
        // 清空与重建过程中列表视图会反复发 LVN_ITEMCHANGED，
        // 期间不能把它的选中状态回同步到 self.selected，否则会先被清空
        self.suppress_selection_events = true;

        unsafe {
            SendMessageW(self.list, LVM_DELETEALLITEMS, 0, 0);
            for position in 0..self.rows.len() {
                self.insert_row(position);
            }
        }

        self.restore_selection();
        self.suppress_selection_events = false;

        self.autosize_columns();
        self.update_sort_arrows();
        crate::ui::status_bar::refresh(self);
    }

    /// 插入一行；第 0 列随插入一起写入，其余列用 `LVM_SETITEMTEXTW` 补。
    unsafe fn insert_row(&self, position: usize) {
        let port = &self.ports[self.rows[position]];

        let mut first = to_wide(&port.port.to_string());
        let mut item: LVITEMW = std::mem::zeroed();
        item.mask = LVIF_TEXT;
        item.iItem = position as i32;
        item.pszText = first.as_mut_ptr();
        let inserted = SendMessageW(self.list, LVM_INSERTITEMW, 0, &item as *const _ as isize);

        let values = [
            port.protocol.clone(),
            port.local_address.clone(),
            port.state.clone(),
            port.pid.to_string(),
            port.process_name.clone(),
            port.process_path.clone(),
        ];
        for (offset, value) in values.iter().enumerate() {
            let mut text = to_wide(value);
            let mut sub: LVITEMW = std::mem::zeroed();
            sub.iSubItem = offset as i32 + 1;
            sub.pszText = text.as_mut_ptr();
            SendMessageW(
                self.list,
                LVM_SETITEMTEXTW,
                inserted as usize,
                &sub as *const _ as isize,
            );
        }
    }

    /// 把 `self.selected` 里的端口在表格中重新选中。
    fn restore_selection(&self) {
        for (position, index) in self.rows.iter().enumerate() {
            let port = &self.ports[*index];
            if !self.selected.contains(&key_of(port)) {
                continue;
            }
            let mut item: LVITEMW = unsafe { std::mem::zeroed() };
            item.mask = LVIF_STATE;
            item.state = LVIS_SELECTED;
            item.stateMask = LVIS_SELECTED;
            unsafe {
                SendMessageW(
                    self.list,
                    LVM_SETITEMSTATE,
                    position,
                    &item as *const _ as isize,
                );
            }
        }
    }

    /// 从列表视图读回选中状态；由 `LVN_ITEMCHANGED` 触发。
    pub(crate) fn sync_selection_from_table(&mut self) {
        if self.suppress_selection_events {
            return;
        }
        let selected: Vec<usize> = self.selected_positions();
        // 先按「上一次的选中集合」判断有没有真变化，避免每次点击都重建整张表
        let unchanged = selected.len() == self.selected.len()
            && selected.iter().all(|position| {
                self.selected
                    .contains(&key_of(&self.ports[self.rows[*position]]))
            });
        if unchanged {
            return;
        }

        self.selected = selected
            .into_iter()
            .map(|position| key_of(&self.ports[self.rows[position]]))
            .collect();
        self.enable_kill_button();
        crate::ui::status_bar::refresh(self);
    }

    /// 当前选中的显示行号。
    pub(crate) fn selected_positions(&self) -> Vec<usize> {
        let mut result = Vec::new();
        let mut current: isize = -1;
        loop {
            current = unsafe {
                SendMessageW(
                    self.list,
                    LVM_GETNEXTITEM,
                    current as usize,
                    LVNI_SELECTED as isize,
                )
            };
            if current < 0 {
                break;
            }
            result.push(current as usize);
        }
        result
    }

    /// 取消表格里的全部选中。`LVM_SETITEMSTATE` 的 iItem 传 -1 表示作用于所有行。
    fn clear_table_selection(&self) {
        let mut item: LVITEMW = unsafe { std::mem::zeroed() };
        item.mask = LVIF_STATE;
        item.state = 0;
        item.stateMask = LVIS_SELECTED;
        unsafe {
            SendMessageW(
                self.list,
                LVM_SETITEMSTATE,
                usize::MAX,
                &item as *const _ as isize,
            );
        }
    }

    /// 表头排序箭头。
    pub(crate) fn update_sort_arrows(&self) {
        let header = unsafe { SendMessageW(self.list, LVM_GETHEADER, 0, 0) as HWND };
        if header.is_null() {
            return;
        }
        let sorted = sort_column_index(self.sort);
        for index in 0..COLUMNS.len() {
            let mut item: HDITEMW = unsafe { std::mem::zeroed() };
            item.mask = HDI_FORMAT;
            unsafe {
                SendMessageW(header, HDM_GETITEMW, index, &mut item as *mut _ as isize);
                item.fmt &= !(HDF_SORTUP | HDF_SORTDOWN);
                if index == sorted {
                    item.fmt |= if self.descending {
                        HDF_SORTDOWN
                    } else {
                        HDF_SORTUP
                    };
                }
                SendMessageW(header, HDM_SETITEMW, index, &item as *const _ as isize);
            }
        }
    }

    /// 列宽自适应内容；最后一列吃掉剩余宽度，避免右侧留白。
    fn autosize_columns(&self) {
        for index in 0..COLUMNS.len() {
            // 注意 LVM_SETCOLUMNWIDTH 返回的是成功与否，宽度要用 LVM_GETCOLUMNWIDTH 单独查
            unsafe {
                SendMessageW(
                    self.list,
                    LVM_SETCOLUMNWIDTH,
                    index,
                    LVSCW_AUTOSIZE_USEHEADER as isize,
                )
            };
        }
        self.stretch_last_column();
    }

    /// 把最后一列拉到表格右边界，避免右侧留白。
    ///
    /// 之所以独立成一个方法，是因为它必须在**控件尺寸确定之后**补跑一次：启动时第一次
    /// 填表发生在布局之前（`PortManagerApp::new` 里就 refresh 了），那一刻列表视图还是
    /// 创建时的 0 宽，`LVSCW_AUTOSIZE_USEHEADER` 算出的「剩余宽度」没有意义，最后一列
    /// 只会拿到内容宽度；而列表视图并不会因为事后被 `MoveWindow` 放大就重新分配列宽，
    /// 于是这块空白会一直留到用户手动拖列为止。所以 `layout()` 每次排完版都要再调一次。
    pub(crate) fn stretch_last_column(&self) {
        let (client_width, _) = crate::ui::client_size(self.list);
        if client_width <= 0 {
            return;
        }
        let mut total = 0;
        for index in 0..COLUMNS.len() {
            total += unsafe { SendMessageW(self.list, LVM_GETCOLUMNWIDTH, index, 0) };
        }
        if total >= client_width as isize {
            return;
        }
        let last = COLUMNS.len() - 1;
        unsafe {
            let current = SendMessageW(self.list, LVM_GETCOLUMNWIDTH, last, 0);
            SendMessageW(
                self.list,
                LVM_SETCOLUMNWIDTH,
                last,
                current + (client_width as isize - total),
            );
        }
    }

    /// 表格右键菜单（§7）。
    pub(crate) fn show_table_menu(&mut self, hit: POINT) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW,
            SetForegroundWindow, TrackPopupMenu, MF_SEPARATOR, MF_STRING, TPM_RIGHTBUTTON, WM_NULL,
        };

        // 右键点在某一行上时，若该行未被选中则先只选中它——
        // 与资源管理器的行为一致，避免误操作到别的进程
        let mut info = LVHITTESTINFO {
            pt: hit,
            flags: 0,
            iItem: -1,
            iSubItem: -1,
            iGroup: -1,
        };
        unsafe {
            SendMessageW(
                self.list,
                LVM_SUBITEMHITTEST,
                0,
                &mut info as *mut _ as isize,
            )
        };
        if info.iItem >= 0 && (info.flags & LVHT_ONITEM) != 0 {
            let position = info.iItem as usize;
            if position < self.rows.len() && !self.selected_positions().contains(&position) {
                self.selected.clear();
                self.selected
                    .insert(key_of(&self.ports[self.rows[position]]));
                self.suppress_selection_events = true;
                self.clear_table_selection();
                self.suppress_selection_events = false;
                self.restore_selection();
                self.enable_kill_button();
                crate::ui::status_bar::refresh(self);
            }
        } else {
            // 点在空白处：没有可操作的行
            return;
        }

        let Some(port) = self.context_port() else {
            return;
        };
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return;
        }
        let items: [(usize, &str); 5] = [
            (ids::MENU_COPY_PORT, "复制端口"),
            (ids::MENU_COPY_PID, "复制 PID"),
            (ids::MENU_DETAIL, "查看详情"),
            (ids::MENU_OPEN_LOCATION, "打开文件位置"),
            (ids::MENU_KILL, "结束进程"),
        ];
        for (index, (id, label)) in items.iter().enumerate() {
            // 「查看详情」之前插一条分隔线
            if index == 2 {
                unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null()) };
            }
            let text = to_wide(label);
            unsafe { AppendMenuW(menu, MF_STRING, *id, text.as_ptr()) };
        }

        let mut cursor = POINT { x: 0, y: 0 };
        unsafe {
            GetCursorPos(&mut cursor);
            // TrackPopupMenu 要求窗口是前台窗口，否则点击别处时菜单不会消失
            SetForegroundWindow(self.window);
            TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON,
                cursor.x,
                cursor.y,
                0,
                self.window,
                ptr::null(),
            );
            PostMessageW(self.window, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
        let _ = port;
    }

    /// 右键菜单当前作用的那条记录：优先取选中集合里最靠前的一条。
    pub(crate) fn context_port(&self) -> Option<crate::models::PortInfo> {
        let positions = self.selected_positions();
        let position = positions.first()?;
        self.rows
            .get(*position)
            .map(|index| self.ports[*index].clone())
    }
}

/// 表头控件的句柄；列表视图尚未创建表头时返回空句柄。
pub(crate) fn header_of(list: HWND) -> HWND {
    if list.is_null() {
        return ptr::null_mut();
    }
    unsafe { SendMessageW(list, LVM_GETHEADER, 0, 0) as HWND }
}

/// 表头被替换下来的原窗口过程（0 表示尚未挂钩）。
///
/// 表头只有一个，进程里也只会装上这一个子类化，所以用单元级静态保存就够了；
/// 换成线程局部或放进 `PortManagerApp` 都只是把同一份地址搬到别处。
/// 用原子量而不是 `static mut`：窗口过程会在消息派发里被重入，裸 `static mut`
/// 的共享引用本身就是 UB（而且 `rust_2024_compatibility` 也会告警）。
static HEADER_ORIGINAL: AtomicIsize = AtomicIsize::new(0);

/// 装表头子类化，并立刻按当前主题重绘一次。
///
/// `SysHeader32` 是深色模式里最难缠的一个：它不发 `NM_CUSTOMDRAW`（子类化前实测
/// 只收到 `HDN_*` 与列表视图发的通知），也不认 `DarkMode_Explorer`，连
/// `WM_ERASEBKGND` 都只在创建时来一次——它在 `WM_PAINT` 里把整套浅色背景画死。
/// 唯一稳定的接管点就是窗口过程本身：自己刷深色底，再按列画标题和排序箭头。
///
/// 只做一次。重复调用只刷新重绘，不重复挂钩。
pub(crate) fn install_header_subclass(list: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWLP_WNDPROC,
    };

    let header = header_of(list);
    if header.is_null() {
        return;
    }

    unsafe {
        if HEADER_ORIGINAL.load(Ordering::Relaxed) == 0 {
            let original = GetWindowLongPtrW(header, GWLP_WNDPROC);
            if original == 0 {
                log::warn!("取表头原窗口过程失败，表头将保持系统浅色外观");
                return;
            }
            HEADER_ORIGINAL.store(original, Ordering::Relaxed);
            SetWindowLongPtrW(header, GWLP_WNDPROC, header_procedure as *const () as isize);
        }
        InvalidateRect(header, ptr::null(), 1);
    }
}

/// 表头的替代窗口过程：接管 `WM_PAINT`，其余原样送回原过程。
///
/// 深色开关放在单元级静态里（[`HEADER_DARK`]），因为窗口过程拿不到 `PortManagerApp`：
/// `GWLP_USERDATA` 已经被主窗口占着。
unsafe extern "system" fn header_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows_sys::Win32::{
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
            SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_LEFT, DT_RIGHT, DT_SINGLELINE,
            DT_VCENTER, PAINTSTRUCT,
        },
        UI::WindowsAndMessaging::WM_PAINT,
    };

    const HDF_CENTER: i32 = 0x0004;
    const HDF_RIGHT: i32 = 0x0002;

    let raw = HEADER_ORIGINAL.load(Ordering::Relaxed);
    if raw == 0 {
        return 0;
    }
    // `WNDPROC` 在 windows-sys 里就是 `Option<unsafe extern "system" fn ...>`，
    // 而 `Option<fn>` 是空指针优化的，非 0 地址一定有值。
    let original = match std::mem::transmute::<
        isize,
        Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    >(raw)
    {
        Some(procedure) => procedure,
        None => return 0,
    };

    // 浅色下不插手，完全交回系统
    if message != WM_PAINT || !HEADER_DARK.load(Ordering::Relaxed) {
        return original(window, message, wparam, lparam);
    }

    let mut paint: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(window, &mut paint);
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(window, &mut client);

    let brush = CreateSolidBrush(crate::ui::COLOR_DARK_BG);
    FillRect(hdc, &client, brush);
    DeleteObject(brush as *mut _);

    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, crate::ui::COLOR_DARK_TEXT);
    let font = SendMessageW(window, WM_GETFONT, 0, 0) as HFONT;
    let previous = SelectObject(hdc, font as *mut _);

    let count = SendMessageW(window, HDM_GETITEMCOUNT, 0, 0) as i32;
    for index in 0..count {
        // HDM_GETITEMRECT 给的是「文字行」的高度，不是整格；竖着要摊到整个客户区，
        // 否则标题会挤在顶部一条里，下面留一道空档。
        let mut cell: RECT = std::mem::zeroed();
        if SendMessageW(
            window,
            HDM_GETITEMRECT,
            index as usize,
            &mut cell as *mut _ as isize,
        ) == 0
        {
            continue;
        }
        cell.top = client.top;
        cell.bottom = client.bottom;

        let mut buffer = [0u16; 128];
        let mut item: HDITEMW = std::mem::zeroed();
        item.mask = HDI_TEXT | HDI_FORMAT;
        item.pszText = buffer.as_mut_ptr();
        item.cchTextMax = buffer.len() as i32;
        if SendMessageW(
            window,
            HDM_GETITEMW,
            index as usize,
            &mut item as *mut _ as isize,
        ) == 0
        {
            continue;
        }
        let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());

        // 每格右侧留出箭头位置，文字才不会压到箭头上
        let mut text_area = cell;
        text_area.left += 6;
        text_area.right -= 18;
        let format = item.fmt;
        let alignment = if format & HDF_CENTER != 0 {
            DT_CENTER
        } else if format & HDF_RIGHT != 0 {
            DT_RIGHT
        } else {
            DT_LEFT
        };
        DrawTextW(
            hdc,
            buffer.as_ptr(),
            length as i32,
            &mut text_area,
            DT_SINGLELINE | DT_VCENTER | alignment,
        );

        // 排序箭头：列表视图的原生箭头也跟着主题一起没了，这里补上
        let arrow = if format & HDF_SORTUP != 0 {
            Some('\u{25B2}')
        } else if format & HDF_SORTDOWN != 0 {
            Some('\u{25BC}')
        } else {
            None
        };
        if let Some(glyph) = arrow {
            let mut points: Vec<u16> = glyph.to_string().encode_utf16().collect();
            let mut arrow_area = cell;
            arrow_area.left = arrow_area.right - 16;
            arrow_area.right -= 4;
            DrawTextW(
                hdc,
                points.as_mut_ptr(),
                points.len() as i32,
                &mut arrow_area,
                DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
            );
        }
    }

    SelectObject(hdc, previous);
    // 真身的 `EndPaint` 收的就是 `const PAINTSTRUCT *`，它只读不写
    EndPaint(window, &paint);
    0
}

/// 表头当前是否按深色绘制；由 [`set_header_dark`] 在主题切换时更新。
static HEADER_DARK: AtomicBool = AtomicBool::new(false);

/// 记下主题并让表头重绘。
pub(crate) fn set_header_dark(list: HWND, dark: bool) {
    HEADER_DARK.store(dark, Ordering::Relaxed);
    let header = header_of(list);
    if !header.is_null() {
        unsafe { InvalidateRect(header, ptr::null(), 1) };
    }
}

/// 表格主体的深色自绘（§9）。
///
/// `SysListView32` 不吃 uxtheme 的深色主题：`SetPreferredAppMode(AllowDark)` 加上
/// `DarkMode_Explorer` 之后它依旧按系统浅色画（实测 Windows 11 26100，无论创建时机、
/// 扩展样式还是重复设置主题都一样）。所以深色下由这里接管行绘制；`SetWindowTheme`
/// 那套对状态栏、编辑框等其它控件仍然有效，只有列表视图与表头要另作处理。
///
/// 表头不走这里：[`install_header_subclass`] 的实测表明 `SysHeader32` 根本不发
/// `NM_CUSTOMDRAW`（它只发 HDN_*），只能换掉窗口过程自己画。
pub(crate) fn handle_custom_draw(app: &PortManagerApp, lparam: LPARAM) -> LRESULT {
    use windows_sys::Win32::UI::Controls::{
        CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, NMLVCUSTOMDRAW,
    };

    // 浅色下一切照旧，交回给控件自己画
    if !app.dark_mode {
        return CDRF_DODEFAULT as LRESULT;
    }

    let draw = unsafe { &mut *(lparam as *mut NMLVCUSTOMDRAW) };

    match draw.nmcd.dwDrawStage {
        // 先声明「每一行还要再问我一次」，否则收不到下面的行级通知
        CDDS_PREPAINT => CDRF_NOTIFYITEMDRAW as LRESULT,
        CDDS_ITEMPREPAINT => {
            unsafe {
                let brush = CreateSolidBrush(crate::ui::COLOR_DARK_BG);
                FillRect(draw.nmcd.hdc, &draw.nmcd.rc, brush);
                DeleteObject(brush as *mut _);
            }
            draw.clrTextBk = crate::ui::COLOR_DARK_BG;
            draw.clrText = crate::ui::COLOR_DARK_TEXT;
            CDRF_DODEFAULT as LRESULT
        }
        _ => CDRF_DODEFAULT as LRESULT,
    }
}

/// 可排序的列号 → 排序字段；不可排序的列返回 `None`。
pub(crate) fn sort_column_of(index: i32) -> Option<SortColumn> {
    match index {
        0 => Some(SortColumn::Port),
        1 => Some(SortColumn::Protocol),
        4 => Some(SortColumn::Pid),
        5 => Some(SortColumn::Name),
        6 => Some(SortColumn::Path),
        _ => None,
    }
}

/// 排序字段 → 列号（用于画排序箭头）。
fn sort_column_index(column: SortColumn) -> usize {
    match column {
        SortColumn::Port => 0,
        SortColumn::Protocol => 1,
        SortColumn::Pid => 4,
        SortColumn::Name => 5,
        SortColumn::Path => 6,
    }
}
