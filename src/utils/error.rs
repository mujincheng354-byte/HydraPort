use std::fmt;

/// 业务错误分类（§9）。服务层用具体变体表达失败原因，UI 层据此决定提示文案与样式，
/// 避免靠匹配错误字符串来猜测原因。
///
/// 实现了 `std::error::Error`，因此可以直接用 `?` 转成 `anyhow::Error` 向上传递。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// 端口枚举失败（IP Helper API 返回了错误码）
    PortQuery { family: i32, code: u32 },
    /// 进程不存在、已退出或无权访问
    ProcessUnavailable { pid: u32 },
    /// 权限不足，需要以管理员身份运行
    PermissionDenied { action: String },
    /// 路径为空或不可用
    InvalidPath,
}

impl AppError {
    /// 端口枚举失败。
    pub fn port_query(family: i32, code: u32) -> Self {
        Self::PortQuery { family, code }
    }

    /// 进程查询或操作失败。
    pub fn process_unavailable(pid: u32) -> Self {
        Self::ProcessUnavailable { pid }
    }

    /// 权限不足。
    pub fn permission_denied(action: impl Into<String>) -> Self {
        Self::PermissionDenied {
            action: action.into(),
        }
    }

    /// 是否为权限不足类错误；UI 用它决定是否给出「以管理员身份运行」的引导。
    pub fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied { .. })
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // 地址族 2 = IPv4，23 = IPv6，出错时提示具体是哪一张表失败
            Self::PortQuery { family, code } => write!(
                formatter,
                "读取地址族 {family} 的端口表失败，错误代码：{code}"
            ),
            Self::ProcessUnavailable { pid } => write!(formatter, "进程 {pid} 不存在或无权访问"),
            // 保持「管理员」字样：这是引导用户提权的关键提示
            Self::PermissionDenied { action } => {
                write!(formatter, "{action}失败；请尝试以管理员身份运行")
            }
            Self::InvalidPath => write!(formatter, "该进程没有可访问的文件路径"),
        }
    }
}

impl std::error::Error for AppError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权限不足类错误的提示必须包含「管理员」，否则用户不知道该怎么办。
    #[test]
    fn permission_error_guides_to_administrator() {
        let error = AppError::permission_denied("结束进程");
        assert!(error.is_permission_denied());
        assert!(error.to_string().contains("管理员"), "实际提示：{error}");
    }

    /// 只有权限类错误会被 UI 当作提权引导。
    #[test]
    fn other_errors_are_not_permission_errors() {
        assert!(!AppError::process_unavailable(4321).is_permission_denied());
        assert!(!AppError::port_query(2, 5).is_permission_denied());
        assert!(!AppError::InvalidPath.is_permission_denied());
    }

    /// 错误可以转成 anyhow::Error 并通过 downcast 取回具体类型。
    #[test]
    fn converts_into_anyhow_and_back() {
        let error: anyhow::Error = AppError::port_query(23, 87).into();
        assert!(
            error.downcast_ref::<AppError>().is_some(),
            "应能取回原始错误类型"
        );
        assert!(error.to_string().contains("87"));
    }

    /// 进程相关错误的提示应带上 PID，便于排查。
    #[test]
    fn process_error_mentions_the_pid() {
        assert!(AppError::process_unavailable(1234)
            .to_string()
            .contains("1234"));
    }
}
