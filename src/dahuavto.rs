use core::fmt;
use hex;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::broadcast::Sender;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp::OwnedWriteHalf, TcpStream},
    sync::{mpsc, Mutex},
};
const DAHUA_PROTO_DHIP: u64 = 0x50494844_00000020;
const DAHUA_REALM_DHIP: u32 = 268632079;
const DEFAULT_KEEPALIVEINTERVAL: u32 = 60;
const HEADER_SIZE: usize = 32;
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub username: String,
    pub password: String,
    pub user_id: String,
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
}

#[derive(Debug)]
pub enum ClientError {
    ConnectionFailed,
    ProtocolError,
    LoginFailed,
    Timeout,
    SerializationError,
    IoError(String),
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::ConnectionFailed => write!(f, "连接失败"),
            ClientError::ProtocolError => write!(f, "协议错误"),
            ClientError::LoginFailed => write!(f, "登录失败"),
            ClientError::Timeout => write!(f, "超时"),
            ClientError::SerializationError => write!(f, "序列化错误"),
            ClientError::IoError(msg) => write!(f, "IO错误: {}", msg),
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(err: std::io::Error) -> Self {
        ClientError::IoError(err.to_string())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceiveParams {
    sid: Option<u64>,
    event_list: Option<Vec<HashMap<String, Value>>>,
    random: Option<String>,
    realm: Option<String>,
    keep_alive_interval: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveRes {
    method: Option<String>,
    params: Option<Value>,
    session: Option<u32>,
    id: Option<u32>,
    error: Option<HashMap<String, Value>>,
    result: Option<Value>,
}

#[derive(Debug)]
pub struct DahuaVTOClient {
    pub config: ClientConfig,
    request_id: AtomicU32,
    session_id: AtomicU32,
    connected: AtomicBool,
    pending_responses: Arc<Mutex<HashMap<u32, mpsc::Sender<ReceiveRes>>>>,
    writer: Mutex<Option<OwnedWriteHalf>>,
}

impl DahuaVTOClient {
    pub fn new(config: ClientConfig) -> Arc<Self> {
        let s = Self {
            config,
            request_id: Default::default(),
            session_id: Default::default(),
            connected: Default::default(),
            pending_responses: Arc::new(Mutex::new(HashMap::new())),
            writer: Mutex::new(None),
        };
        Arc::new(s)
    }

    pub async fn connect(self: Arc<Self>) -> Result<(), ClientError> {
        let stream_result = tokio::time::timeout(
            self.config.connect_timeout,
            TcpStream::connect((self.config.host.as_str(), self.config.port)),
        )
        .await;

        let stream = match stream_result {
            Ok(Ok(stream)) => stream,
            Ok(Err(_)) => return Err(ClientError::ConnectionFailed),
            Err(_) => return Err(ClientError::Timeout), // 连接超时
        };

        // 分离读写端
        let (reader, writer) = stream.into_split();
        // 设置写入端
        *self.writer.lock().await = Some(writer);
        // 标记为已连接
        self.connected.store(true, Ordering::SeqCst);
        // 启动接收任务
        tokio::spawn({
            let this = Arc::clone(&self);
            this.handle_connection(reader)
        });
        // 发送初始登录请求
        match self
            .send(
                &json!({
                    "method": "global.login",
                    "params": {
                        "clientType": "",
                        "ipAddr": "(null)",
                        "loginType": "Direct"
                    }
                }),
                true,
            )
            .await
        {
            Ok(res) => self.login(res.unwrap()).await,
            Err(e) => return Err(e),
        }?;
        Ok(())
    }

    async fn handle_connection(self: Arc<Self>, mut reader: OwnedReadHalf) {
        let mut chunk = Vec::new();
        let mut buf = [0u8; 40996];

        while self.connected.load(Ordering::SeqCst) {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    log::warn!("连接关闭:");
                    break;
                } // 连接关闭
                Ok(n) => {
                    chunk.extend_from_slice(&buf[..n]);
                    if let Err(e) = self.clone().process_data(&mut chunk).await {
                        log::warn!("数据处理错误: {:?}", e);
                    }
                }
                Err(e) => {
                    log::warn!("读取数据错误: {:?}", e);
                }
            }
        }
        // 清理连接状态
        self.connected.store(false, Ordering::SeqCst);
        self.pending_responses.lock().await.clear();
    }


    async fn process_data(self: Arc<Self>, chunk: &mut Vec<u8>) -> Result<(), ClientError> {
        while chunk.len() >= HEADER_SIZE {
            let proto = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
            if proto != DAHUA_PROTO_DHIP {
                return Err(ClientError::ProtocolError);
            }
            let packet_len = u64::from_le_bytes(chunk[24..32].try_into().unwrap()) as usize;
            let total_len = HEADER_SIZE + packet_len;

            if chunk.len() < total_len {
                return Ok(());
            }
            let packet = &chunk[HEADER_SIZE..total_len];
            log::info!("接收数据: {:?} {}/{}", String::from_utf8(packet.to_vec().clone()),chunk.len(),total_len);
            // 处理消息
            let message: ReceiveRes = serde_json::from_slice(packet).map_err(|_| ClientError::SerializationError)?;
            let mut tail = total_len;
            if chunk.len() - tail == HEADER_SIZE {
                tail += HEADER_SIZE;
            }
            *chunk = chunk[tail..].to_vec();
            self.clone().receive(message).await?;
        }
        Ok(())
    }

    async fn receive(self: Arc<Self>, message: ReceiveRes) -> Result<(), ClientError> {
        // 错误处理
        if let Some(ref error) = message.error {
            if let Some(code) = error.get("code").and_then(|c| c.as_f64()) {
                let pending = self.pending_responses.lock().await;
                let id = self.request_id.load(Ordering::SeqCst);
                if let Some(tx) = pending.get(&id) {
                    match tx.send(message).await {
                        Ok(_) => return Ok(()),
                        Err(_) => {
                            return Err(ClientError::ProtocolError);
                        }
                    }
                } else {
                    log::warn!("未找到请求: {:?}", message);
                    return Err(ClientError::ProtocolError);
                }
            }
            return Err(ClientError::LoginFailed);
        }
        self.handle_response(message).await?;
        Ok(())
    }

    async fn login(self: Arc<Self>, message: ReceiveRes) -> Result<(), ClientError> {
        // 解析参数
        let params: ReceiveParams =
            serde_json::from_value(message.params.ok_or(ClientError::ProtocolError)?)
                .map_err(|_| ClientError::SerializationError)?;
        if let Some(session) = message.session {
            self.session_id.store(session, Ordering::SeqCst);
        }
        let Some(random) = params.random else {
            return Err(ClientError::ProtocolError);
        };
        let Some(realm) = params.realm else {
            return Err(ClientError::ProtocolError);
        };
        match self.send(
            &json!({
                "method": "global.login",
                "params": {
                    "userName": self.config.username,
                    "password": Self::hashed_password(&self.config.username, &self.config.password, &random, &realm),
                    "clientType": "",
                    "ipAddr": "(null)",
                    "loginType": "Direct"
                },
                "session": self.session_id.load(Ordering::SeqCst)
            }),
            true,
        ).await? {
            Some(res) => {
                // 确定心跳间隔
                let interval = if let Some(params) = res.params {
                    serde_json::from_value::<ReceiveParams>(params)
                        .ok()
                        .and_then(|p| p.keep_alive_interval)
                        .unwrap_or(DEFAULT_KEEPALIVEINTERVAL)
                } else {
                    DEFAULT_KEEPALIVEINTERVAL
                };
                tokio::spawn({
                    self.start_heartbeat(interval)
                });
                Ok(())
            },
            None => return Err(ClientError::LoginFailed),
        }
    }

    fn hashed_password(username: &str, password: &str, random: &str, realm: &str) -> String {
        // 第一次哈希
        let first = format!("{}:{}:{}", username, realm, password);
        let mut hasher = Md5::new();
        hasher.update(first.as_bytes());
        let first_hash = hex::encode(hasher.finalize()).to_uppercase();

        // 第二次哈希
        let second = format!("{}:{}:{}", username, random, first_hash);
        let mut hasher = Md5::new();
        hasher.update(second.as_bytes());
        hex::encode(hasher.finalize()).to_uppercase()
    }

    pub async fn sub_event(
        self: Arc<DahuaVTOClient>,
        user_id: &str,
        tx: Sender<Arc<DahuaVTOClient>>,
        is_callback: bool,
    ) -> Result<(), ClientError> {
        // 初始化事件订阅
        self.send(
            &json!({
                "method": "eventManager.factory.instance",
                "params": {}
            }),
            false,
        )
        .await?;
        let id = self.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        let rx = self
            .send_recv(
                id,
                &json!({
                    "method": "eventManager.attach",
                    "params": {"codes": ["All"]}
                }),
                true,
            )
            .await?;
        // 等待响应（如果需要）
        if let Some(mut rx) = rx {
            loop {
                match rx.recv().await {
                    None => {
                        self.pending_responses.lock().await.remove(&id);
                        return Err(ClientError::IoError(
                            "Pending response channel closed".to_string(),
                        ));
                    }
                    Some(res) => {
                        if let Some(method) = res.method.as_deref() {
                            match method {
                                "client.notifyEventStream" => {
                                    // 解析事件参数
                                    let params = match &res.params {
                                        Some(p) => p,
                                        None => {
                                            log::error!("Event notification missing params");
                                            return Ok(());
                                        }
                                    };

                                    // 提取事件列表
                                    let events = match params.get("eventList") {
                                        Some(Value::Array(events)) => events,
                                        _ => {
                                            log::error!(
                                                "Invalid or missing eventList in notification"
                                            );
                                            return Ok(());
                                        }
                                    };

                                    // 处理每个事件
                                    for event in events {
                                        // 提取事件基本信息
                                        let event_code = event
                                            .get("Code")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("Unknown");
                                        // 根据事件类型进行不同处理
                                        match event_code {
                                            // 门铃呼叫事件
                                            "Invite" => {
                                            // "CallNoAnswered" => {
                                                if let Some(data) = event.get("Data") {
                                                    let caller_id = data
                                                        .get("UserID")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("Unknown");
                                                    if user_id == caller_id && is_callback == true {
                                                        log::info!("大华vto Invite事件 {} 监听{} 通知状态{}",caller_id,user_id,is_callback);
                                                        tx.send(self.clone()).map_err(|_| {
                                                            ClientError::IoError(
                                                                "Failed to send client to tx"
                                                                    .to_string(),
                                                            )
                                                        })?;
                                                    }
                                                }
                                            }
                                            // 其他事件
                                            _ => {
                                                // log::debug!("Unhandled event type: {}", event_code);
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        } else {
            Err(ClientError::ConnectionFailed)
        }
    }

    async fn start_heartbeat(self: Arc<Self>, interval: u32) {
        let mut interval_timer = tokio::time::interval(Duration::from_secs(std::cmp::min(
            interval as u64,
            DEFAULT_KEEPALIVEINTERVAL as u64,
        )));
        log::info!("{:?} VTO 连接成功！开启心跳！", self.config.host);
        loop {
            interval_timer.tick().await;
            // 检查连接状态
            if !self.connected.load(Ordering::SeqCst) {
                log::info!("{:?} 连接关闭！停止心跳", self.config.host);
                break;
            }
            let this = self.clone();
            match self
                .send(
                    &json!({
                        "method": "global.keepAlive",
                        "params": {
                            "timeout": interval,
                            "action": true
                        },
                        "session": self.session_id.load(Ordering::SeqCst)
                    }),
                    true,
                )
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => log::error!("{:?} 心跳包没返回", self.config.host),
                Err(e) => {
                    log::error!("{:?} 发生心跳包失败，连接关闭: {:?}", self.config.host, e);
                    this.close().await;
                }
            }
        }
    }

    async fn handle_response(self: Arc<Self>, message: ReceiveRes) -> Result<(), ClientError> {
        if let Some(id) = message.id {
            let pending = self.pending_responses.lock().await;
            if let Some(tx) = pending.get(&id) {
                match tx.send(message).await {
                    Ok(_) => Ok(()),
                    Err(_) => Ok(()),
                }
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    pub async fn send_recv(
        &self,
        id: u32,
        message: &Value,
        response_expected: bool,
    ) -> Result<Option<mpsc::Receiver<ReceiveRes>>, ClientError> {
        // 检查连接状态
        if !self.connected.load(Ordering::SeqCst) {
            return Err(ClientError::ConnectionFailed);
        }
        // 生成请求ID
        let session = self.session_id.load(Ordering::SeqCst);
        // 创建消息副本并设置ID和session
        let mut msg = message.clone();
        msg["id"] = json!(id);
        if session > 0 {
            msg["session"] = json!(session);
        }
        // 序列化消息
        let data = serde_json::to_vec(&msg).map_err(|_| ClientError::SerializationError)?;
        // 创建响应通道
        let (_, rx) = if response_expected {
            let (tx, rx) = mpsc::channel(1);
            self.pending_responses.lock().await.insert(id, tx.clone());
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        // log::info!("发送数据: {:?}", String::from_utf8(data.clone()));
        // 准备协议头部 (32字节)
        let mut header = [0u8; HEADER_SIZE];
        header[0..8].copy_from_slice(&DAHUA_PROTO_DHIP.to_le_bytes());
        header[8..12].copy_from_slice(&(session as u32).to_le_bytes()); // session_id作为u32
        header[12..16].copy_from_slice(&(id as u32).to_le_bytes()); // request_id作为u32

        // 写入数据长度 (两个64位位置都写入相同值)
        let data_len = data.len() as u64;
        header[16..24].copy_from_slice(&data_len.to_le_bytes());
        header[24..32].copy_from_slice(&data_len.to_le_bytes());

        // 获取写入锁
        let mut writer_lock = self.writer.lock().await;
        if let Some(writer) = writer_lock.as_mut() {
            // 写入头部
            writer.write_all(&header).await?;

            // 写入数据体
            writer.write_all(&data).await?;

            // 确保数据刷新
            writer.flush().await?;
        } else {
            return Err(ClientError::IoError(
                "TCP writer not initialized".to_string(),
            ));
        }
        Ok(rx)
    }

    pub async fn send(
        &self,
        message: &Value,
        response_expected: bool,
    ) -> Result<Option<ReceiveRes>, ClientError> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst) + 1;
        let rx = self.send_recv(id, message, response_expected).await?;
        // 等待响应（如果需要）
        if let Some(mut rx) = rx {
            match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                Ok(Some(res)) => {
                    self.pending_responses.lock().await.remove(&id);
                    Ok(Some(res))
                }
                Ok(None) => {
                    self.pending_responses.lock().await.remove(&id);
                    Err(ClientError::IoError(
                        "Response channel closed unexpectedly".to_string(),
                    ))
                }
                Err(_) => {
                    self.pending_responses.lock().await.remove(&id);
                    Err(ClientError::Timeout)
                }
            }
        } else {
            Ok(None)
        }
    }

    /// 取消当前通话
    pub async fn cancel_call(self: Arc<Self>) -> Result<(), ClientError> {
        self.send(
            &json!({
                "method": "console.runCmd",
                "params": {
                    "command": "hc"
                }
            }),
            true,
        )
        .await?;
        Ok(())
    }

    /// 开门并取消通话
    pub async fn open_door_and_cancel_call(
        self: Arc<Self>,
        short_number: &str,
    ) -> Result<(), ClientError> {
        // 先开门
        self.clone().open_door(short_number).await?;
        // 等待1秒
        tokio::time::sleep(Duration::from_secs(1)).await;
        // 取消通话
        if let Err(e) = self.cancel_call().await {
            log::error!("Failed to cancel call: {:?}", e);
        }
        Ok(())
    }

    /// 开门操作
    pub async fn open_door(self: Arc<Self>, short_number: &str) -> Result<(), ClientError> {
        self.clone()
            .access_control_instance(|object_id| async move {
                // 发送开门命令
                let response = self
                    .send(
                        &json!({
                            "method": "accessControl.openDoor",
                            "object": object_id,
                            "params": {
                                "DoorIndex": 0,
                                "ShortNumber": short_number
                            }
                        }),
                        true,
                    )
                    .await?;

                // 解析响应结果
                if let Some(resp) = response {
                    if let Some(result) = resp.result {
                        // 检查开门是否成功
                        if let Some(success) = result.as_bool() {
                            if success {
                                return Ok(());
                            } else {
                                return Err(ClientError::IoError(
                                    "Open door failed: result is false".to_string(),
                                ));
                            }
                        }
                        // 处理其他可能的result类型
                        if result.is_null() {
                            // 有些设备可能返回null表示成功
                            return Ok(());
                        }
                    }
                }
                Err(ClientError::IoError(
                    "Open door failed: invalid response".to_string(),
                ))
            })
            .await
    }

    // fn extract_floor_number(room_code: &str) -> i32 {
    //     let len = room_code.len();
    //     if len <= 2 {
    //         return 0;
    //     }
    //     if len == 3 {
    //         if let Some(first_char) = room_code.chars().next() {
    //             if let Some(digit) = first_char.to_digit(10) {
    //                 return digit as i32;
    //             }
    //         }
    //     }
    //     if len >= 4 {
    //         let start_index = len.saturating_sub(4);
    //         let end_index = start_index + 2;
    //         if let Some(floor_str) = room_code.get(start_index..end_index) {
    //             if let Ok(floor) = floor_str.parse::<i32>() {
    //                 return floor;
    //             }
    //         }
    //     }
    //     0
    // }
    //
    // pub async fn call_lift(
    //     self: Arc<Self>,
    //     src: i32,
    //     dest_floor: i32,
    // ) -> Result<Option<ReceiveRes>, ClientError> {
    //     self.clone().access_control_instance(|object_id| async move {
    //         self.send(
    //             &json!({
    //                 "method": "accessControl.callLift",
    //                 "object": object_id,
    //                 "params": {
    //                     "Src": src,
    //                     "DestFloor": dest_floor,
    //                     "CallLiftCmd": "",
    //                     "CallLiftAction": ""
    //                 }
    //             }),
    //             true,
    //         )
    //             .await
    //     })
    //         .await
    // }

    /// 访问控制实例管理辅助方法
    /// 创建accessControl实例，执行回调函数，然后销毁实例
    pub async fn access_control_instance<F, Fut, T>(
        self: Arc<Self>,
        callback: F,
    ) -> Result<T, ClientError>
    where
        F: FnOnce(i64) -> Fut,
        Fut: Future<Output = Result<T, ClientError>>,
    {
        // 创建accessControl实例
        let instance_response = self
            .send(
                &json!({
                    "method": "accessControl.factory.instance",
                    "params": {
                        "channel": 0
                    }
                }),
                true,
            )
            .await?;

        // 提取对象ID
        let object_id = instance_response
            .and_then(|resp| resp.result)
            .and_then(|result| result.as_i64())
            .ok_or_else(|| ClientError::IoError("Failed to get object ID".to_string()))?;

        if object_id == 0 {
            return Err(ClientError::IoError("Invalid object ID".to_string()));
        }

        // 使用defer模式确保实例被销毁
        let result = callback(object_id).await;

        // 无论回调成功与否，都尝试销毁实例
        let _ = self
            .send(
                &json!({
                    "method": "accessControl.destroy",
                    "object": object_id
                }),
                false,
            )
            .await;

        result
    }

    pub async fn close(self: Arc<Self>) {
        self.connected.store(false, Ordering::SeqCst);
        self.pending_responses.lock().await.clear();
        // 关闭写入端
        let mut writer_lock = self.writer.lock().await;
        if let Some(mut writer) = writer_lock.take() {
            if let Err(e) = writer.shutdown().await {
                log::error!("Failed to shutdown writer: {:?}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_divide() -> Result<(), String> {
        let config = ClientConfig {
            username: "admin".to_string(),
            password: "admin123".to_string(),
            host: "192.168.13.1".to_string(),
            user_id: "".to_string(),
            port: 80,
            connect_timeout: Duration::from_secs(5),
        };
        let client = DahuaVTOClient::new(config);
        // 连接设备
        if let Err(e) = client.clone().connect().await {
            log::error!("连接失败: {:?}", e);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        client
            .clone()
            .open_door_and_cancel_call("")
            .await
            .expect("TODO: panic message");
        // 断开连接
        client.clone().close().await;
        Ok(())
    }
}
