// Release 版本不弹出控制台窗口，调试版本保留以便查看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod models;
mod services;
mod ui;
mod utils;

fn main() -> anyhow::Result<()> {
    // 尽量靠前打点，让「启动耗时」覆盖窗口与全部控件的创建过程
    utils::timing::mark_start();
    // §8 启动流程第 1 步：先装好日志，后续任何一步出问题都有据可查
    utils::logger::init();
    log::info!("HydraPort 启动，版本 {}", env!("CARGO_PKG_VERSION"));

    let result = ui::run();
    match &result {
        Ok(()) => log::info!(
            "HydraPort 退出，总运行时长 {} ms",
            utils::timing::elapsed_ms()
        ),
        Err(error) => log::error!("HydraPort 异常退出：{error:#}"),
    }
    result
}
