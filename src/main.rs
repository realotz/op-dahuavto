mod bafa_cloud;
mod config;
mod dahuavto;
mod gpio;
mod http_server;
mod mqtt;
mod sip;

use crate::config::AppConfig;
use log::LevelFilter;
use simplelog::{ColorChoice, Config, TermLogger, TerminalMode};
use std::net::SocketAddr;
use syslog::Facility;
use tokio::net::TcpListener;

// 调试模式专用常量
#[cfg(debug_assertions)]
const DEVICE_CONFIG_FILE: &str = "./files/dahuavto.conf";

// 发布模式专用常量
#[cfg(not(debug_assertions))]
const DEVICE_CONFIG_FILE: &str = "/etc/config/dahuavto";

fn init_syslog() -> Result<(), Box<dyn std::error::Error>> {
    syslog::init(
        Facility::LOG_USER,
        log::LevelFilter::Info,
        Some("dahuavto-rs"),
    )?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(target_env = "musl") {
        init_syslog().expect("无法初始化 syslog");
    } else {
        // 只输出到控制台
        TermLogger::init(
            LevelFilter::Info,
            Config::default(),
            TerminalMode::Stderr,
            ColorChoice::Auto,
        )
        .expect("初始化日志失败");
    }
    // 初始化服务器状态
    let app_config = AppConfig::from_file(DEVICE_CONFIG_FILE).expect("没有找到配置文件");
    let app = http_server::new(app_config.clone()).await;
    // 启动服务器
    let listen_addr = app_config
        .listen
        .parse::<SocketAddr>()
        .unwrap_or_else(|_| "0.0.0.0:2066".parse().expect("默认地址解析失败"));
    let listener = TcpListener::bind(listen_addr).await?;
    log::info!("服务已运行 {:?}", listen_addr);
    axum::serve(listener, app).await?;
    Ok(())
}
