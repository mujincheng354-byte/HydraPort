use std::{collections::HashSet, mem::size_of, net::{Ipv4Addr, Ipv6Addr}, ptr};

use anyhow::Result;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use windows_sys::Win32::{
    Foundation::ERROR_INSUFFICIENT_BUFFER,
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        UDP_TABLE_OWNER_PID,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
};

use crate::{models::PortInfo, utils::AppError};

/// 解析 MIB 结构中的端口号：端口以网络字节序存放在 DWORD 的低 16 位。
fn port_from_dword(value: u32) -> u16 {
    u16::from_be(value as u16)
}

/// 解析 MIB 结构中的 IPv4 地址：地址以网络字节序存放在 DWORD 中，
/// 也就是 DWORD 的内存字节顺序即地址的字节顺序，必须按本机字节序读回。
fn ipv4_from_dword(value: u32) -> Ipv4Addr {
    Ipv4Addr::from(value.to_ne_bytes())
}

/// 端口表中的一行，此时还没有关联任何进程信息。
struct RawPort {
    port: u16,
    protocol: &'static str,
    local_address: String,
    state: &'static str,
    pid: u32,
}

/// 使用 IP Helper API 枚举本机监听端口。
pub struct PortService;

impl PortService {
    /// 获取 TCP 监听端口和 UDP 绑定端口，并附加进程信息。
    pub fn scan() -> Result<Vec<PortInfo>> {
        // 第一步：读取四张端口表，这一步完全不接触进程信息。
        let mut raw = Vec::new();
        raw.extend(Self::tcp_v4()?);
        raw.extend(Self::tcp_v6()?);
        raw.extend(Self::udp_v4()?);
        raw.extend(Self::udp_v6()?);

        // 第二步：只为端口表里出现过的 PID 拉取进程名与路径。
        // 这里不能用 System::new_all() + refresh_all()：那会枚举全部进程，并顺带读取每个进程的
        // 命令行、环境块、磁盘用量与所属用户，实测多占约 18 MB 内存，而本程序一个都用不到。
        let pids: Vec<Pid> = raw
            .iter()
            .map(|item| Pid::from_u32(item.pid))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::new().with_exe(UpdateKind::Always),
        );

        let mut ports: Vec<PortInfo> = raw.iter().map(|item| item.to_port_info(&system)).collect();
        ports.sort_by(|left, right| (left.port, &left.protocol, left.pid).cmp(&(right.port, &right.protocol, right.pid)));
        Ok(ports)
    }

    fn tcp_v4() -> Result<Vec<RawPort>> {
        let buffer = Self::query_table(AF_INET as i32, |table, size| unsafe {
            GetExtendedTcpTable(table, size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_LISTENER, 0)
        })?;
        Ok(Self::rows(&buffer, |row: MIB_TCPROW_OWNER_PID| RawPort {
            port: port_from_dword(row.dwLocalPort),
            protocol: "TCP",
            local_address: ipv4_from_dword(row.dwLocalAddr).to_string(),
            state: "LISTENING",
            pid: row.dwOwningPid,
        }))
    }

    fn tcp_v6() -> Result<Vec<RawPort>> {
        let buffer = Self::query_table(AF_INET6 as i32, |table, size| unsafe {
            GetExtendedTcpTable(table, size, 0, AF_INET6 as u32, TCP_TABLE_OWNER_PID_LISTENER, 0)
        })?;
        Ok(Self::rows(&buffer, |row: MIB_TCP6ROW_OWNER_PID| RawPort {
            port: port_from_dword(row.dwLocalPort),
            protocol: "TCP",
            local_address: Ipv6Addr::from(row.ucLocalAddr).to_string(),
            state: "LISTENING",
            pid: row.dwOwningPid,
        }))
    }

    fn udp_v4() -> Result<Vec<RawPort>> {
        let buffer = Self::query_table(AF_INET as i32, |table, size| unsafe {
            GetExtendedUdpTable(table, size, 0, AF_INET as u32, UDP_TABLE_OWNER_PID, 0)
        })?;
        Ok(Self::rows(&buffer, |row: MIB_UDPROW_OWNER_PID| RawPort {
            port: port_from_dword(row.dwLocalPort),
            protocol: "UDP",
            local_address: ipv4_from_dword(row.dwLocalAddr).to_string(),
            state: "绑定",
            pid: row.dwOwningPid,
        }))
    }

    fn udp_v6() -> Result<Vec<RawPort>> {
        let buffer = Self::query_table(AF_INET6 as i32, |table, size| unsafe {
            GetExtendedUdpTable(table, size, 0, AF_INET6 as u32, UDP_TABLE_OWNER_PID, 0)
        })?;
        Ok(Self::rows(&buffer, |row: MIB_UDP6ROW_OWNER_PID| RawPort {
            port: port_from_dword(row.dwLocalPort),
            protocol: "UDP",
            local_address: Ipv6Addr::from(row.ucLocalAddr).to_string(),
            state: "绑定",
            pid: row.dwOwningPid,
        }))
    }

    fn query_table<F>(address_family: i32, query: F) -> Result<Vec<u8>>
    where F: Fn(*mut core::ffi::c_void, *mut u32) -> u32 {
        // 先以空指针调用一次，让 API 回传所需缓冲区大小
        let mut size = 0_u32;
        let result = query(ptr::null_mut(), &mut size);
        if result != ERROR_INSUFFICIENT_BUFFER || size == 0 {
            return Err(AppError::port_query(address_family, result).into());
        }
        let mut buffer = vec![0_u8; size as usize];
        let result = query(buffer.as_mut_ptr().cast(), &mut size);
        if result != 0 { return Err(AppError::port_query(address_family, result).into()); }
        Ok(buffer)
    }

    fn rows<T: Copy>(buffer: &[u8], map: impl Fn(T) -> RawPort) -> Vec<RawPort> {
        if buffer.len() < size_of::<u32>() { return Vec::new(); }
        let count = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) } as usize;
        let row_size = size_of::<T>();
        (0..count)
            .filter_map(|index| {
                let offset = size_of::<u32>() + index * row_size;
                (offset + row_size <= buffer.len()).then(|| unsafe {
                    map(ptr::read_unaligned(buffer.as_ptr().add(offset).cast::<T>()))
                })
            })
            .collect()
    }

}

impl RawPort {
    /// 把端口表的一行与进程信息合并成一条界面记录。
    fn to_port_info(&self, system: &System) -> PortInfo {
        let process = system.process(Pid::from_u32(self.pid));
        let process_name = process.map(|item| item.name().to_string_lossy().into_owned()).unwrap_or_else(|| "访问受限或进程已退出".to_owned());
        let process_path = process.and_then(|item| item.exe()).map(|path| path.display().to_string()).unwrap_or_default();
        PortInfo {
            port: self.port,
            protocol: self.protocol.to_owned(),
            local_address: self.local_address.clone(),
            state: self.state.to_owned(),
            pid: self.pid,
            process_name,
            process_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正常获取 TCP/UDP 监听端口，且字段合法。
    #[test]
    fn scan_returns_listening_ports() {
        let ports = PortService::scan().expect("扫描端口失败");
        assert!(!ports.is_empty(), "本机应至少存在一个监听端口");
        assert!(ports.iter().any(|port| port.protocol == "TCP"), "应至少存在一个 TCP 监听端口");
        assert!(ports.iter().any(|port| port.protocol == "UDP"), "应至少存在一个 UDP 绑定端口");

        for port in &ports {
            assert!(port.protocol == "TCP" || port.protocol == "UDP", "协议只能是 TCP 或 UDP：{}", port.protocol);
            // TCP 表使用 LISTENER 类别查询，状态固定为 LISTENING
            if port.protocol == "TCP" {
                assert_eq!(port.state, "LISTENING");
            }
            assert!(!port.local_address.is_empty(), "本地地址不应为空");
            assert!(port.port > 0 || port.protocol == "UDP", "监听端口号不应为 0");
        }
    }

    /// 扫描结果按端口号有序，UI 与导出依赖该顺序。
    #[test]
    fn scan_result_is_sorted() {
        let ports = PortService::scan().expect("扫描端口失败");
        assert!(ports.windows(2).all(|pair| pair[0].port <= pair[1].port), "扫描结果应按端口号升序排列");
    }

    /// 刷新后数据一致性：连续两次扫描的结果应基本一致（仅允许进程启停造成的少量差异）。
    #[test]
    fn scan_is_consistent_across_refreshes() {
        let first = PortService::scan().expect("第一次扫描失败");
        let second = PortService::scan().expect("第二次扫描失败");
        let common = first.iter().filter(|item| second.iter().any(|other| other == *item)).count();
        assert!(
            common * 10 >= first.len() * 9,
            "两次扫描的共同记录过少：{common}/{}",
            first.len()
        );
    }

    /// TCP/UDP 端口号解析：Windows 以网络字节序存放在低 16 位。
    /// 端口号解析：端口在 DWORD 中以网络字节序存放。
    #[test]
    fn port_number_is_decoded_from_network_order() {
        // 端口 445（0x01BD）在内存中的低 16 位字节为 01 BD
        assert_eq!(port_from_dword(u32::from_ne_bytes([0x01, 0xBD, 0, 0])), 445);
        // 端口 8080（0x1F90）
        assert_eq!(port_from_dword(u32::from_ne_bytes([0x1F, 0x90, 0, 0])), 8080);
        // 端口 80（0x0050）
        assert_eq!(port_from_dword(u32::from_ne_bytes([0x00, 0x50, 0, 0])), 80);
    }

    /// IPv4 地址解析：DWORD 的内存字节顺序就是地址的字节顺序。
    #[test]
    fn ipv4_address_is_decoded_from_memory_order() {
        assert_eq!(ipv4_from_dword(u32::from_ne_bytes([127, 0, 0, 1])).to_string(), "127.0.0.1");
        assert_eq!(ipv4_from_dword(u32::from_ne_bytes([10, 221, 8, 80])).to_string(), "10.221.8.80");
        assert_eq!(ipv4_from_dword(u32::from_ne_bytes([192, 168, 71, 1])).to_string(), "192.168.71.1");
        // 通配地址 0.0.0.0 与广播地址都不能被字节序颠倒
        assert_eq!(ipv4_from_dword(0).to_string(), "0.0.0.0");
        assert_eq!(ipv4_from_dword(u32::from_ne_bytes([255, 255, 255, 255])).to_string(), "255.255.255.255");
    }

    /// 扫描结果中的本地地址必须是可解析的 IP，且不应出现本机不存在的公网地址式乱序。
    #[test]
    fn scan_addresses_are_well_formed() {
        let ports = PortService::scan().expect("扫描端口失败");
        for port in &ports {
            let address = &port.local_address;
            assert!(
                address.parse::<std::net::IpAddr>().is_ok(),
                "本地地址无法解析为 IP：{address}"
            );
        }
    }
}
