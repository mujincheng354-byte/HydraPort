use crate::models::PortInfo;

/// 端口列表的过滤与精确查询逻辑；不依赖界面层，便于单元测试。
pub struct PortQuery;

impl PortQuery {
    /// 判断单条记录是否命中关键字（端口号、PID 或进程名，不区分大小写）；空关键字命中全部。
    pub fn matches(port: &PortInfo, keyword: &str) -> bool {
        let keyword = keyword.trim().to_lowercase();
        if keyword.is_empty() {
            return true;
        }
        port.port.to_string().contains(&keyword)
            || port.pid.to_string().contains(&keyword)
            || port.process_name.to_lowercase().contains(&keyword)
    }

    /// 按关键字与精确端口号过滤，返回命中的下标；`exact_port` 为 `None` 时不限制端口号。
    ///
    /// 界面层只需要一份下标列表就能完成排序与绘制，避免每帧克隆整张表（§12）。
    pub fn filter_indices(ports: &[PortInfo], keyword: &str, exact_port: Option<u16>) -> Vec<usize> {
        ports
            .iter()
            .enumerate()
            .filter(|(_, port)| Self::matches(port, keyword) && exact_port.is_none_or(|value| port.port == value))
            .map(|(index, _)| index)
            .collect()
    }

    /// 精确查询占用指定端口号的全部记录（同一端口可能同时被 IPv4/IPv6 或多个进程占用）。
    pub fn find_by_port(ports: &[PortInfo], port: u16) -> Vec<&PortInfo> {
        ports.iter().filter(|item| item.port == port).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(port: u16, protocol: &str, pid: u32, name: &str) -> PortInfo {
        PortInfo {
            port,
            protocol: protocol.to_owned(),
            local_address: "0.0.0.0".to_owned(),
            state: "LISTENING".to_owned(),
            pid,
            process_name: name.to_owned(),
            process_path: String::new(),
        }
    }

    fn sample_list() -> Vec<PortInfo> {
        vec![
            sample(8080, "TCP", 4321, "nginx.exe"),
            sample(53, "UDP", 900, "svchost.exe"),
            sample(3306, "TCP", 4321, "mysqld.exe"),
        ]
    }

    /// 搜索：端口号、PID、进程名均可匹配，且进程名不区分大小写。
    #[test]
    fn matches_by_port_pid_and_name() {
        let item = sample(8080, "TCP", 4321, "nginx.exe");
        assert!(PortQuery::matches(&item, "8080"));
        assert!(PortQuery::matches(&item, "4321"));
        assert!(PortQuery::matches(&item, "NGINX"));
        assert!(PortQuery::matches(&item, "  nginx  "));
        assert!(!PortQuery::matches(&item, "3306"));
    }

    /// 搜索：空关键字不应过滤掉任何记录。
    #[test]
    fn empty_keyword_matches_all() {
        let ports = sample_list();
        assert_eq!(PortQuery::filter_indices(&ports, "", None).len(), ports.len());
        assert_eq!(PortQuery::filter_indices(&ports, "   ", None).len(), ports.len());
    }

    /// 搜索：按进程名过滤，同一进程的多个端口应全部命中。
    #[test]
    fn filter_by_process_name() {
        let ports = sample_list();
        let found = PortQuery::filter_indices(&ports, "mysqld", None);
        assert_eq!(found.len(), 1);
        assert_eq!(ports[found[0]].port, 3306);
    }

    /// 精确查询：存在的端口应能定位到对应进程。
    #[test]
    fn find_existing_port() {
        let ports = sample_list();
        let found = PortQuery::find_by_port(&ports, 8080);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].process_name, "nginx.exe");
        assert_eq!(found[0].pid, 4321);
    }

    /// 精确查询：不存在的端口应返回空结果。
    #[test]
    fn find_missing_port_returns_empty() {
        let ports = sample_list();
        assert!(PortQuery::find_by_port(&ports, 9999).is_empty());
        assert!(PortQuery::filter_indices(&ports, "", Some(9999)).is_empty());
    }

    /// 精确端口与关键字同时生效（取交集）。
    #[test]
    fn exact_port_combines_with_keyword() {
        let ports = sample_list();
        assert_eq!(PortQuery::filter_indices(&ports, "nginx", Some(8080)).len(), 1);
        assert!(PortQuery::filter_indices(&ports, "nginx", Some(3306)).is_empty());
    }

    /// 下标过滤：返回的下标必须能直接索引回原切片，且顺序与原始顺序一致。
    #[test]
    fn filter_indices_are_sorted_and_indexable() {
        let ports = sample_list();
        let indices = PortQuery::filter_indices(&ports, "32", None);
        // 8080 与 4321 都含 "32"，对应下标 0 和 2；顺序应与原切片一致
        assert_eq!(indices, vec![0, 2]);
        assert_eq!(ports[indices[0]].port, 8080);
    }
}
