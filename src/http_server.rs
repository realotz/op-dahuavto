use crate::bafa_cloud::init_start_bafa;
use crate::config::{AppConfig, DeviceConfig};
use crate::dahuavto::{ClientConfig, ClientError, DahuaVTOClient};
use crate::gpio::{call_notify, init_gpio, watch_gpio};
use crate::sip::SipServer;
use crate::DEVICE_CONFIG_FILE;
use axum::body::Body;
use axum::http::Request;
use axum::middleware::{from_fn, Next};
use axum::{
    extract::{Query, State},
    http,
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::ops::Deref;
use std::process::Command;
use std::time::SystemTime;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::broadcast;
use tokio::sync::broadcast::Sender;
use tokio::{
    sync::RwLock,
    time::{sleep, Duration},
};
use crate::mqtt::init_start_mqtt;

// 服务器状态结构
#[derive(Clone)]
pub struct ServerState {
    pub(crate) vto_clients: Arc<RwLock<HashMap<String, Arc<DahuaVTOClient>>>>,
    pub(crate) cfg: Arc<AppConfig>,
    pub(crate) last_call_client: Arc<RwLock<Option<Arc<DahuaVTOClient>>>>,
    pub(crate) last_call_time: Arc<RwLock<Option<SystemTime>>>,
}

#[derive(Debug)]
pub enum ServerError {
    DoorOperationError(),
}

impl ServerState {
    pub(crate) async fn open_call_door(&self) -> Result<(), ServerError> {
        // 获取最后一次呼叫时间和客户端
        let last_call_time = self.last_call_time.read().await;
        let last_call_client = self.last_call_client.read().await;

        // 检查是否有有效的呼叫记录
        if let (Some(call_time), Some(client)) = (&*last_call_time, &*last_call_client) {
            // 计算时间差
            let elapsed = call_time
                .elapsed()
                .map_err(|_| ServerError::DoorOperationError())?;
            // 判断是否在30秒内
            if elapsed <= Duration::from_secs(30) {
                let client = client.clone();
                // 执行开门操作
                if let Err(_) = client.open_door_and_cancel_call(&self.cfg.userid).await {
                    return Err(ServerError::DoorOperationError());
                }
                return Ok(());
            }
        }
        Err(ServerError::DoorOperationError())
    }
}
pub(crate) async fn new(app_config: Arc<AppConfig>) -> Router {
    // 初始化服务器状态
    let state = ServerState {
        vto_clients: Arc::new(RwLock::new(HashMap::new())),
        cfg: app_config.clone(),
        last_call_client: Arc::new(RwLock::new(None)),
        last_call_time: Arc::new(RwLock::new(None)),
    };
    if let Err(e) = init_gpio(&app_config.gpio_out, true).await {
        log::error!("GPIO初始化失败: {}", e);
    }
    let (tx, _) = broadcast::channel::<Arc<DahuaVTOClient>>(1);
    for device in &app_config.clone().vtos {
        match init_vto_client(
            tx.clone(),
            state.clone(),
            device.clone(),
            app_config.clone(),
        )
        .await
        {
            Ok(client) => {
                state
                    .vto_clients
                    .write()
                    .await
                    .insert(device.id.clone(), client);
            }
            Err(e) => {
                log::error!("初始化失败 {} {}", device.ip, e);
            }
        }
    }
    if let Err(e) = subscribe_call_evnet(tx.clone(), state.clone()).await {
        log::error!("订阅呼叫事件失败 {}", e);
    }
    if let Err(e) = init_start_mqtt(app_config.clone(), state.clone(), tx.clone()).await {
        log::error!("初始化MQTT失败 {}", e);
    }
    if let Err(e) = init_start_bafa(app_config.clone(), state.clone(), tx.clone()).await {
        log::error!("初始化巴法云失败 {:?}", e);
    }
    if app_config.sip_interface != "" {
        let mut clients = HashMap::new();
        for (_, ct) in state.vto_clients.read().await.iter() {
            clients.insert(ct.config.host.clone(), ct.clone());
        }
        let ss = SipServer::new(app_config.sip_interface.clone(), tx, clients);
        if let Err(e) = ss.run() {
            log::error!("监听网卡失败 {:?}", e);
        }
    }
    log::info!("创建路由");
    // 创建路由
    Router::new()
        .route("/", get(root_handler))
        .route("/api/open", get(handle_open_door))
        .route("/api/test", get(handle_test))
        .route(
            "/api/config",
            get(get_config).post(update_config).put(update_config),
        )
        .with_state(state.clone())
        .layer(from_fn({
            let config = app_config.clone();
            move |req: Request<Body>, next: Next| {
                let config = config.clone(); // 每次调用都 clone
                async move {
                    let auth_header = req
                        .headers()
                        .get(http::header::AUTHORIZATION)
                        .and_then(|h| h.to_str().ok());

                    match auth_header {
                        Some(auth_value) if auth_value.starts_with("Basic ") => {
                            let encoded = &auth_value["Basic ".len()..];
                            if let Ok(decoded) = STANDARD.decode(encoded) {
                                if let Ok(cred_str) = String::from_utf8(decoded) {
                                    let parts: Vec<&str> = cred_str.splitn(2, ':').collect();
                                    if parts.len() == 2 {
                                        let (username, password) = (parts[0], parts[1]);
                                        if username == config.username
                                            && password == config.password
                                        {
                                            return Ok(next.run(req).await); // ✅ 这里是 async 块内，可以 await
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }

                    let mut response = StatusCode::UNAUTHORIZED.into_response();
                    response.headers_mut().insert(
                        http::header::WWW_AUTHENTICATE,
                        "Basic realm=\"Access to API\"".parse().unwrap(),
                    );
                    Err(response)
                }
            }
        }))
}

async fn subscribe_call_evnet(
    tx: Sender<Arc<DahuaVTOClient>>,
    state: ServerState,
) -> Result<(), ClientError> {
    let mut call_sub = tx.subscribe();
    let state_clone = state.clone();
    tokio::spawn(async move {
        while let Ok(cc) = call_sub.recv().await {
            log::info!("收到呼叫事件 {}", cc.config.host);
            if state.cfg.auto_open {
                log::info!("执行自动开门 {}", cc.config.host);
                if let Err(e) = cc.clone().open_door_and_cancel_call("").await {
                    log::error!("自动开门失败 {}", e);
                }
            }
            {
                // 更新最后一次呼叫的客户端
                let mut last_client = state_clone.last_call_client.write().await;
                *last_client = Some(cc.clone());
            }
            {
                // 更新最后一次呼叫的时间
                let mut last_time = state_clone.last_call_time.write().await;
                *last_time = Some(SystemTime::now());
            }
            // 如果有GPIO输出配置，触发通知
            if !state_clone.cfg.gpio_out.is_empty() {
                if let Err(e) = call_notify(&state_clone.cfg.gpio_out).await {
                    log::error!("GPIO通知失败: {}", e);
                }
            }
        }
    });
    Ok(())
}

async fn init_vto_client(
    tx: Sender<Arc<DahuaVTOClient>>,
    state: ServerState,
    device: DeviceConfig,
    cfg: Arc<AppConfig>,
) -> Result<Arc<DahuaVTOClient>, ClientError> {
    let tx = tx.clone();
    let client = DahuaVTOClient::new(ClientConfig {
        username: device.name.to_string(),
        password: device.password.to_string(),
        host: device.ip.to_string(),
        port: device.port.clone(),
        user_id: device.userid.clone(),
        connect_timeout: Duration::from_secs(5),
    });
    if device.gpio != 0 {
        if let Err(e) = init_gpio(&device.gpio.to_string(), false).await {
            log::error!("GPIO初始化失败: {}", e);
        }
        let client_clone = client.clone();
        match watch_gpio(device.gpio).await {
            Ok(mut rx) => {
                tokio::spawn(async move {
                    while let Some(value) = rx.recv().await {
                        log::info!("GPIO值变化{}: {}", device.gpio, value);
                        match state.clone().open_call_door().await {
                            Ok(_) => {
                                log::info!("打开了呼叫的门");
                            }
                            Err(_) => {
                                if let Err(e) =
                                    client_clone.clone().open_door_and_cancel_call("").await
                                {
                                    log::error!("{}打开门失败 {}", client_clone.config.host, e);
                                }
                            }
                        }
                    }
                    log::info!("watch_gpio任务执行完成");
                });
            }
            Err(e) => {
                log::error!("GPIO监听失败: {}", e);
            }
        }
    }
    let sc = client.clone();
    let mut user_id = cfg.userid.to_string();
    if device.userid != "" {
        user_id = device.userid.clone();
    }
    log::info!("连接设备任务初始化 {} 监听 {}", device.ip, user_id);
    tokio::spawn(async move {
        loop {
            let txd = tx.clone();
            match sc.clone().connect().await {
                Ok(_) => {
                    // 连接成功后订阅事件
                    if let Err(e) = sc
                        .clone()
                        .sub_event(&user_id, txd, cfg.sip_interface.is_empty())
                        .await
                    {
                        log::warn!("事件订阅失败: {:?}", e);
                        sleep(Duration::from_secs(10)).await;
                        continue;
                    }
                }
                Err(e) => {
                    log::warn!("连接失败: {:?}, 10秒后重试...", e);
                    sleep(Duration::from_secs(10)).await;
                }
            }
        }
    });

    Ok(client)
}

// 编译时嵌入 HTML 文件
const INDEX_HTML: &str = include_str!("ui.html");

// 根路由处理器
async fn root_handler() -> Html<&'static str> {
    // 这里应该读取并返回UI模板文件
    Html(INDEX_HTML)
}

// 开门接口
async fn handle_open_door(
    State(state): State<ServerState>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let id = match params.get("id") {
        Some(id) => id,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "请求参数错误"})),
            )
        }
    };

    let vto_clients = state.vto_clients.read().await;
    let client = match vto_clients.get(id) {
        Some(client) => client.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "VTO未找到"})),
            )
        }
    };
    // 执行开门操作
    if let Err(res) = client.open_door_and_cancel_call(&state.cfg.userid).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("执行开门操作错误: {:?}", res)})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"result": "success"})),
    )
}

// 测试接口
async fn handle_test(State(state): State<ServerState>) -> impl IntoResponse {
    // 测试GPIO通知
    if !state.cfg.gpio_out.is_empty() {
        if let Err(e) = call_notify(&state.cfg.gpio_out).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("callNotify测试错误: {}", e)})),
            );
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"result": "success"})),
    )
}

// 获取配置
async fn get_config(State(state): State<ServerState>) -> Json<AppConfig> {
    Json(state.cfg.deref().clone())
}

// 更新配置
async fn update_config(
    State(state): State<ServerState>,
    Json(mut new_cfg): Json<AppConfig>,
) -> impl IntoResponse {
    // 保留原密码如果新密码为空
    if new_cfg.password.is_empty() {
        new_cfg.password = state.cfg.password.clone();
    }
    AppConfig::to_file(&new_cfg, DEVICE_CONFIG_FILE).unwrap();
    // 异步重启服务
    tokio::spawn(async {
        sleep(Duration::from_secs(1)).await;
        if let Err(e) = Command::new("/etc/init.d/dahuavto-rs")
            .arg("restart")
            .output()
        {
            log::error!("重启服务失败: {}", e);
        }
    });
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "result": "success",
            "message": "配置更新成功"
        })),
    )
}
