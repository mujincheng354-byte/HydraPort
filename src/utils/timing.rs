//! 启动计时（§1.3 启动速度）。只记录一个进程内的起点，不参与业务逻辑。

use std::{sync::OnceLock, time::Instant};

/// 进程启动时刻；由 `main` 在最开始处打点。
static START: OnceLock<Instant> = OnceLock::new();

/// 记录进程启动时刻；重复调用只保留第一次。
pub fn mark_start() {
    let _ = START.set(Instant::now());
}

/// 距离 [`mark_start`] 已经过去的毫秒数；未打点时返回 0。
pub fn elapsed_ms() -> u128 {
    START.get().map_or(0, |start| start.elapsed().as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 打点之后应能读到一个非负的耗时；未打点时不 panic。
    #[test]
    fn elapsed_is_available_after_mark() {
        // 其它测试可能已经打过点，这里只要求结果可用且单调不减
        mark_start();
        let first = elapsed_ms();
        let second = elapsed_ms();
        assert!(second >= first);
    }
}
