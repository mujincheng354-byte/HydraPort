//! 应用状态与业务编排。
//!
//! 这里只放「状态 + 业务动作」，控件句柄与具体界面操作在 [`crate::ui`] 的各子模块里，
//! 通过 `impl PortManagerApp` 拼在一起（§3、§12）。

use std::collections::BTreeSet;

use chrono::{DateTime, Local};
use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{HBRUSH, HFONT},
    UI::WindowsAndMessaging::PostMessageW,
};

use crate::{
    models::PortInfo,
    services::{AutostartService, PortQuery, PortService, ProcessService, TrayEvent, TrayService},
    ui,
    utils::is_administrator,
};

/// 端口协议；用于构造行标识，避免每帧为每一行分配字符串。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("UDP") { Self::Udp } else { Self::Tcp }
    }
}

/// 表格行的唯一标识：同一端口号可能被不同协议、不同进程同时占用。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct PortKey {
    port: u16,
    protocol: Protocol,
    pid: u32,
}

pub(crate) fn key_of(port: &PortInfo) -> PortKey {
    PortKey { port: port.port, protocol: Protocol::parse(&port.protocol), pid: port.pid }
}

/// 表格可排序的列。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortColumn {
    Port,
    Protocol,
    Pid,
    Name,
    Path,
}

/// 工具栏上的控件句柄。
pub(crate) struct ToolbarControls {
    pub(crate) search: HWND,
    pub(crate) exact: HWND,
    pub(crate) query: HWND,
    pub(crate) refresh: HWND,
    pub(crate) export: HWND,
    pub(crate) kill: HWND,
    pub(crate) theme: HWND,
    pub(crate) settings: HWND,
    /// 「搜索：」与「精确端口：」两个静态标签。
    ///
    /// 这里必须显式保存句柄：静态控件没有通知，无法靠 ID 找回，而按子窗口顺序去取
    /// 又会依赖 Z 序（`GetWindow(GW_CHILD)` 的顺序与创建顺序相反）。
    pub(crate) labels: [HWND; 2],
}

pub struct PortManagerApp {
    /// 主窗口句柄；托盘线程通过它投递事件
    pub(crate) window: HWND,
    /// 端口表格（列表视图）
    pub(crate) list: HWND,
    /// 底部状态栏
    pub(crate) status: HWND,
    /// 工具栏控件
    pub(crate) toolbar: ToolbarControls,
    /// 界面字体，随 DPI 变化重建
    pub(crate) font: HFONT,
    /// 当前主题的窗口背景画刷
    pub(crate) background: HBRUSH,
    /// 当前 DPI
    pub(crate) dpi: u32,

    /// 全量端口数据（刷新时整体替换）
    pub(crate) ports: Vec<PortInfo>,
    /// 当前显示的行：`ports` 的下标。只在数据或筛选条件变化时重算（§12 避免不必要的 clone）
    pub(crate) rows: Vec<usize>,
    rows_dirty: bool,
    /// 重建表格期间挂起选中同步：清空再填充的过程会反复触发 `LVN_ITEMCHANGED`，
    /// 若此时回读选中状态，会把 `self.selected` 提前清空（§8）。
    pub(crate) suppress_selection_events: bool,
    /// 状态栏三段的文本；深色主题下状态栏是自绘的，绘制时要靠它取回文字
    pub(crate) status_parts: [String; ui::status_bar::PART_COUNT],
    pub(crate) search: String,
    pub(crate) exact_port: String,
    /// 当前选中的端口；始终与列表视图的选中状态保持一致
    pub(crate) selected: BTreeSet<PortKey>,
    pub(crate) last_refresh: Option<DateTime<Local>>,
    pub(crate) message: String,
    pub(crate) administrator: bool,
    pub(crate) sort: SortColumn,
    pub(crate) descending: bool,
    pub(crate) dark_mode: bool,
    /// 关闭主窗口时最小化到托盘而不是退出
    pub(crate) close_to_tray: bool,
    /// 开机自启开关的当前状态
    pub(crate) autostart: bool,
    /// 结束进程时是否同时结束子进程
    pub(crate) kill_tree: bool,
    /// 系统托盘；创建失败时为 `None`，此时不允许「关闭到托盘」
    tray: Option<TrayService>,
    /// 托盘菜单选择了「退出」
    pub(crate) quit_requested: bool,
}

impl PortManagerApp {
    /// 创建全部控件并完成首次扫描。`window` 必须已经创建好。
    ///
    /// 控件创建失败时返回错误：调用方负责销毁已经建好的窗口。控件句柄会被
    /// 布局、绘制、消息处理反复取用，让一个空句柄混进 `PortManagerApp` 里，
    /// 后面每一处都得再判一次空，而且失败点离根因越来越远。
    pub fn new(window: HWND) -> anyhow::Result<Self> {
        let dpi = ui::dpi_of(window);
        let font = ui::create_ui_font(dpi);
        let dark_mode = crate::utils::system_uses_dark_theme();
        let background = ui::create_background_brush(dark_mode);

        let toolbar = ui::toolbar::create(window)?;
        let list = ui::table::create(window)?;
        let status = ui::status_bar::create(window)?;

        let mut app = Self {
            window,
            list,
            status,
            toolbar,
            font,
            background,
            dpi,
            ports: Vec::new(),
            rows: Vec::new(),
            rows_dirty: true,
            suppress_selection_events: false,
            status_parts: std::array::from_fn(|_| String::new()),
            search: String::new(),
            exact_port: String::new(),
            selected: BTreeSet::new(),
            last_refresh: None,
            message: String::new(),
            administrator: is_administrator(),
            sort: SortColumn::Port,
            descending: false,
            dark_mode,
            close_to_tray: true,
            autostart: AutostartService::is_enabled(),
            kill_tree: true,
            tray: None,
            quit_requested: false,
        };

        ui::apply_font(window, font);
        ui::apply_theme(window, dark_mode);
        app.sync_theme_button();
        app.enable_kill_button();

        // 托盘回调运行在托盘线程上：只投递一个消息唤醒界面线程，不直接碰界面状态。
        // 此时 window 一定已经存在，所以回调里可以直接投递。
        let target = window as isize;
        app.tray = TrayService::start(move |event| {
            let code = event.code();
            unsafe { PostMessageW(target as HWND, ui::WM_TRAY_EVENT, code, 0) };
        });
        // 托盘不可用时不能隐藏窗口，否则用户再也打不开它
        app.close_to_tray = app.tray_is_active();

        log::info!(
            "程序启动：管理员={}，托盘={}，开机自启={}",
            app.administrator,
            app.tray_is_active(),
            app.autostart
        );

        app.refresh();
        app.sync_window_title();
        // refresh() 刚写过一条「已刷新」提示，这里覆盖掉它：非管理员时提权提示更重要（§1.1）
        if !app.administrator {
            app.set_message(
                "普通用户模式：结束系统进程可能失败，建议右键「以管理员身份运行」".to_owned(),
            );
        }
        Ok(app)
    }

    /// 重新扫描端口；刷新后丢弃已经消失的选中项（§8 刷新流程）。
    pub(crate) fn refresh(&mut self) {
        match PortService::scan() {
            Ok(ports) => {
                log::info!("端口扫描完成，共 {} 条", ports.len());
                self.ports = ports;
                self.last_refresh = Some(Local::now());
                let alive: BTreeSet<PortKey> = self.ports.iter().map(key_of).collect();
                self.selected.retain(|key| alive.contains(key));
                self.message = format!("端口列表已刷新，共 {} 条", self.ports.len());
                self.mark_rows_dirty();
            }
            // 扫描失败时保留上一次的结果，只更新提示信息
            Err(error) => {
                log::error!("端口扫描失败：{error:#}");
                self.message = format!("刷新失败：{error:#}");
            }
        }
        // 数据换了，表格必须跟着重建，否则界面会停在上一次的结果上
        self.rebuild_rows();
    }

    /// 标记当前显示的行需要重算。
    pub(crate) fn mark_rows_dirty(&mut self) {
        self.rows_dirty = true;
    }

    /// 依据搜索条件与排序列重算 `rows`，并把结果同步到表格；没有变化时不做任何事。
    pub(crate) fn rebuild_rows(&mut self) {
        if !self.rows_dirty {
            return;
        }
        let exact = self.exact_port.trim().parse::<u16>().ok();
        // 过滤逻辑放在 services 层，UI 这里只负责排序与下标收集
        let mut rows: Vec<usize> = PortQuery::filter_indices(&self.ports, &self.search, exact);
        rows.sort_by(|left, right| {
            let (left, right) = (&self.ports[*left], &self.ports[*right]);
            let order = match self.sort {
                SortColumn::Port => left.port.cmp(&right.port),
                SortColumn::Protocol => left.protocol.cmp(&right.protocol),
                SortColumn::Pid => left.pid.cmp(&right.pid),
                SortColumn::Name => left.process_name.cmp(&right.process_name),
                SortColumn::Path => left.process_path.cmp(&right.process_path),
            };
            if self.descending { order.reverse() } else { order }
        });
        self.rows = rows;
        self.rows_dirty = false;
        self.reload_table();
    }

    /// 当前显示的行对应的端口数据；用于导出等一次性操作。
    pub(crate) fn filtered_ports(&self) -> Vec<&PortInfo> {
        self.rows.iter().map(|index| &self.ports[*index]).collect()
    }

    pub(crate) fn toggle_sort(&mut self, column: SortColumn) {
        if self.sort == column { self.descending = !self.descending; } else { self.sort = column; self.descending = false; }
        self.mark_rows_dirty();
    }

    /// 当前选中的端口（在全量列表中查找，不受过滤条件影响）。
    pub(crate) fn selected_ports(&self) -> Vec<PortInfo> {
        self.ports.iter().filter(|port| self.selected.contains(&key_of(port))).cloned().collect()
    }

    /// 批量结束进程；同一 PID 只结束一次，并跳过本程序自身。
    pub(crate) fn kill_ports(&mut self, ports: &[PortInfo]) {
        let current_pid = std::process::id();
        let mut pids: Vec<u32> = Vec::new();
        for port in ports {
            if port.pid != current_pid && !pids.contains(&port.pid) { pids.push(port.pid); }
        }
        if pids.is_empty() {
            self.set_message("没有可结束的进程".to_owned());
            return;
        }

        let mut failures = Vec::new();
        let mut permission_denied = false;
        for pid in &pids {
            match ProcessService::kill_process(*pid, self.kill_tree) {
                Ok(()) => log::info!("已结束进程 {pid}（结束子进程={}）", self.kill_tree),
                Err(error) => {
                    log::warn!("结束进程 {pid} 失败：{error:#}");
                    // 权限类错误需要额外引导用户提权（§6.3）
                    permission_denied |= error
                        .downcast_ref::<crate::utils::AppError>()
                        .is_some_and(|item| item.is_permission_denied());
                    failures.push(format!("{error:#}"));
                }
            }
        }
        let summary = if failures.is_empty() {
            format!("已结束 {} 个进程", pids.len())
        } else if permission_denied {
            format!(
                "成功结束 {} 个，失败 {} 个；部分进程需要以管理员身份运行才能结束",
                pids.len() - failures.len(),
                failures.len()
            )
        } else {
            format!(
                "成功结束 {} 个，失败 {} 个：{}",
                pids.len() - failures.len(),
                failures.len(),
                failures.join("；")
            )
        };
        self.refresh();
        self.set_message(summary);
    }

    /// 导出当前过滤结果到 CSV（使用系统「另存为」对话框）。
    pub(crate) fn export_csv(&mut self) {
        let Some(path) = crate::utils::save_file_dialog("监听端口.csv") else {
            self.set_message("已取消导出".to_owned());
            return;
        };
        // csv 的序列化错误与 io 的 flush 错误类型不同，统一收敛为 anyhow::Error
        let result = (|| -> anyhow::Result<()> {
            let mut writer = csv::Writer::from_path(&path)?;
            for item in self.filtered_ports() { writer.serialize(item)?; }
            writer.flush()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                log::info!("已导出 CSV：{}", path.display());
                self.set_message(format!("已导出到：{}", path.display()));
            }
            Err(error) => {
                log::error!("导出 CSV 失败：{error:#}");
                self.set_message(format!("导出失败：{error:#}"));
            }
        }
    }

    /// 系统托盘是否可用；托盘创建失败时相关开关需要禁用。
    pub(crate) fn tray_is_active(&self) -> bool {
        self.tray.as_ref().is_some_and(TrayService::is_active)
    }

    /// 在资源管理器中定位进程文件（§7 右键菜单）。
    pub(crate) fn open_file_location(&mut self, path: &str) {
        let message = match ProcessService::open_file_location(path) {
            Ok(()) => "已打开文件位置".to_owned(),
            Err(error) => format!("打开文件位置失败：{error:#}"),
        };
        self.set_message(message);
    }

    /// 切换开机自启；写入注册表后提示结果。
    pub(crate) fn set_autostart(&mut self, enabled: bool) {
        match AutostartService::set_enabled(enabled) {
            Ok(()) => {
                self.autostart = enabled;
                self.set_message(if enabled { "已开启开机自启".to_owned() } else { "已关闭开机自启".to_owned() });
                log::info!("开机自启已{}", if enabled { "开启" } else { "关闭" });
            }
            Err(error) => {
                // 写入失败时回读真实状态，避免开关停在错误的位置
                self.autostart = AutostartService::is_enabled();
                log::error!("设置开机自启失败：{error:#}");
                self.set_message(format!("设置开机自启失败：{error:#}"));
            }
        }
    }

    /// 更新提示信息并同步到状态栏。
    pub(crate) fn set_message(&mut self, message: String) {
        self.message = message;
        ui::status_bar::refresh(self);
    }

    /// 处理托盘线程投递过来的事件。
    pub(crate) fn handle_tray_event(&mut self, event: TrayEvent) {
        log::info!("处理托盘事件：{event:?}");
        match event {
            TrayEvent::Show => ui::main_window::show(self),
            TrayEvent::Refresh => {
                self.refresh();
                self.rebuild_rows();
                self.set_message(self.message.clone());
            }
            TrayEvent::Exit => self.quit_requested = true,
        }
    }

    /// 关闭主窗口时的行为：开启「最小化到托盘」且托盘可用时隐藏窗口，否则退出（§1.2）。
    pub(crate) fn handle_close_request(&mut self) {
        if self.close_to_tray && self.tray_is_active() {
            ui::main_window::hide(self);
            self.set_message("已最小化到系统托盘，单击或双击托盘图标可重新打开".to_owned());
        } else {
            self.quit_requested = true;
        }
    }

    /// 退出前清理托盘图标，避免通知区域留下残留。
    pub(crate) fn shutdown(&mut self) {
        if let Some(tray) = self.tray.take() { tray.shutdown(); }
        ui::delete_object(self.font as *mut _);
        ui::delete_object(self.background as *mut _);
        log::info!("程序退出");
    }
}
