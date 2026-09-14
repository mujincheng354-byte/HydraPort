use std::{collections::HashSet, ffi::OsStr, process::Command};

use anyhow::Result;
use chrono::{DateTime, Local};
use sysinfo::{Pid, ProcessesToUpdate, System};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS},
        Threading::{OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE},
    },
};

use crate::utils::AppError;

/// 进程的详细信息，用于「查看详情」窗口。
#[derive(Debug, Clone)]
pub struct ProcessDetail {
    pub name: String,
    pub command_line: Option<String>,
    pub parent_pid: Option<u32>,
    pub memory_bytes: u64,
    pub start_time: Option<DateTime<Local>>,
}

/// 负责结束进程及打开进程文件所在位置。
pub struct ProcessService;

impl ProcessService {
    /// 查询单个进程的详细信息；进程不存在或权限不足时返回错误。
    pub fn get_process_info(pid: u32) -> Result<ProcessDetail> {
        let target = Pid::from_u32(pid);
        // 只刷新目标进程，避免枚举全部进程带来的开销
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        let process = system.process(target).ok_or_else(|| AppError::process_unavailable(pid))?;

        let command_line = (!process.cmd().is_empty()).then(|| process.cmd().join(OsStr::new(" ")).to_string_lossy().into_owned());
        let started_at = process.start_time();
        // start_time 为 0 表示系统未提供该信息
        let start_time = if started_at == 0 { None } else { DateTime::from_timestamp(started_at as i64, 0).map(|time| time.with_timezone(&Local)) };

        Ok(ProcessDetail {
            name: process.name().to_string_lossy().into_owned(),
            command_line,
            parent_pid: process.parent().map(|parent| parent.as_u32()),
            memory_bytes: process.memory(),
            start_time,
        })
    }

    /// 结束指定进程；启用 `kill_tree` 时会先结束其所有子进程。
    pub fn kill_process(pid: u32, kill_tree: bool) -> Result<()> {
        if kill_tree {
            for child_pid in Self::descendants(pid)? { Self::terminate(child_pid)?; }
        }
        Self::terminate(pid)
    }

    /// 在资源管理器中定位可执行文件。
    pub fn open_file_location(path: &str) -> Result<()> {
        if path.is_empty() { return Err(AppError::InvalidPath.into()); }
        Command::new("explorer.exe").arg(format!("/select,{path}")).spawn()?;
        Ok(())
    }

    fn terminate(pid: u32) -> Result<()> {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() { return Err(AppError::permission_denied(format!("打开进程 {pid}")).into()); }
        let result = unsafe { TerminateProcess(handle, 1) };
        unsafe { CloseHandle(handle); }
        if result == 0 { return Err(AppError::permission_denied(format!("结束进程 {pid}")).into()); }
        Ok(())
    }

    fn descendants(root_pid: u32) -> Result<Vec<u32>> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE { return Err(AppError::permission_denied("创建进程快照").into()); }
        let mut entries = Vec::new();
        // PROCESSENTRY32W 未实现 Default，必须清零后再写入 dwSize
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
        while has_entry {
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
        }
        unsafe { CloseHandle(snapshot); }
        let mut descendants = Vec::new();
        let mut parents = HashSet::from([root_pid]);
        while !parents.is_empty() {
            let current: HashSet<u32> = entries.iter().filter_map(|(pid, parent)| parents.contains(parent).then_some(*pid)).collect();
            descendants.extend(current.iter().copied());
            parents = current;
        }
        descendants.reverse();
        Ok(descendants)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 启动一个存活约 60 秒的 ping 进程，用于结束进程相关测试；输出丢弃以免干扰测试报告。
    fn spawn_idle_process() -> std::process::Child {
        Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("无法创建测试进程")
    }

    /// 获取进程基本信息：查询自身应成功。
    #[test]
    fn get_process_info_of_self() {
        let detail = ProcessService::get_process_info(std::process::id()).expect("查询自身进程失败");
        // 服务返回的进程名应与测试程序自身的可执行文件名一致
        let expected = std::env::current_exe().expect("无法获取当前可执行文件路径");
        let expected_name = expected.file_name().expect("可执行文件路径缺少文件名").to_string_lossy().to_lowercase();
        assert_eq!(detail.name.to_lowercase(), expected_name, "进程名与可执行文件名不一致");
        assert!(detail.memory_bytes > 0, "内存占用应为正数");
        assert!(detail.start_time.is_some(), "应能获取启动时间");
        assert!(detail.parent_pid.is_some(), "应能获取父进程 PID");
    }

    /// 查询不存在的进程应返回错误而不是 panic。
    #[test]
    fn get_process_info_of_missing_pid_fails() {
        assert!(ProcessService::get_process_info(u32::MAX - 1).is_err());
    }

    /// 杀死普通用户进程。
    #[test]
    fn kill_user_process() {
        let mut child = spawn_idle_process();
        let pid = child.id();
        // 等待进程真正启动，避免刚 spawn 就被结束导致的竞态
        std::thread::sleep(Duration::from_millis(300));

        ProcessService::kill_process(pid, false).expect("结束测试进程失败");
        let status = child.wait().expect("等待测试进程退出失败");
        assert!(!status.success(), "被强制结束的进程不应返回成功退出码");
        assert!(ProcessService::get_process_info(pid).is_err(), "进程结束后不应再查询到");
    }

    /// 结束进程树：子进程应一并被结束。
    #[test]
    fn kill_process_tree_terminates_children() {
        // 通过 cmd 启动 ping，使 ping 成为 cmd 的子进程
        let mut child = Command::new("cmd")
            .args(["/C", "ping", "-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("无法创建测试进程");
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(500));

        let children = ProcessService::descendants(pid).expect("获取子进程失败");
        assert!(!children.is_empty(), "cmd 应至少启动一个子进程");

        ProcessService::kill_process(pid, true).expect("结束进程树失败");
        child.wait().expect("等待测试进程退出失败");
        for child_pid in children {
            // TerminateProcess 返回后进程对象还会短暂残留在系统进程列表里，
            // 因此轮询等待而不是立即断言，否则测试会随机失败。
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while ProcessService::get_process_info(child_pid).is_ok() {
                assert!(std::time::Instant::now() < deadline, "子进程 {child_pid} 未被结束");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    /// 无法打开的进程（权限不足 / 不存在）应给出引导用户提权的提示。
    #[test]
    fn kill_unopenable_process_reports_permission_hint() {
        // PID 0 为系统空闲进程、u32::MAX 附近为不存在的 PID，两者都无法打开
        for pid in [0, u32::MAX - 1] {
            let error = ProcessService::kill_process(pid, false).expect_err("结束不存在的进程不应成功");
            let message = format!("{error:#}");
            assert!(message.contains("管理员"), "错误提示应引导用户以管理员身份运行，实际为：{message}");
        }
    }

    /// 打开文件位置：路径为空时应报错而不是启动资源管理器。
    #[test]
    fn open_file_location_rejects_empty_path() {
        assert!(ProcessService::open_file_location("").is_err());
    }
}
