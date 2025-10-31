use crate::config::AppConfig;
use crate::dahuavto::DahuaVTOClient;
use crate::http_server::ServerState;
use anyhow::{anyhow, Result};
use rumqttc::{self, AsyncClient, ClientError, Event, Incoming, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast::Sender;
use tokio::time::Duration;

// 常量定义
const BAFA_MQTT_BROKER: &str = "bemfa.com";
const BAFA_API_BASE: &str = "https://pro.bemfa.com";
const DEF_BAFA_CALL_NOTIFY: &str = "callnotify1006";

// 结构体定义
#[derive(Debug, Serialize, Deserialize)]
struct CreateTopicRequest {
    name: String,
    uid: String,
    topic: String,
    #[serde(rename = "type")]
    type_: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetTopicRes {
    name: String,
    msg: String,
    online: bool,
    share: bool,
    group: String,
    room: String,
    time: String,
    unix: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct BafaApiResponse<T> {
    code: i32,
    msg: String,
    data: Option<T>,
}
#[derive(Debug, Serialize, Deserialize)]
struct BafaApiResponse2 {
    code: i32,
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeleteTopicRequest {
    uid: String,
    topic: String,
    #[serde(rename = "type")]
    type_: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListTopicRow {
    data: Vec<TopicData>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TopicData {
    topic: String,
    #[serde(rename = "type")]
    type_: i32,
    share: bool,
    time: String,
    unix: i64,
    online: bool,
    name: String,
    room: String,
    msg: String,
}
// 巴法云客户端实现
#[derive(Clone)]
pub struct BafaClient {}

pub async fn init_start_bafa(
    config: Arc<AppConfig>,
    state: ServerState,
    tx: Sender<Arc<DahuaVTOClient>>,
) -> Result<(), ClientError> {
    if config.bafa_key.is_empty() {
        return Ok(());
    }
    let api_key = config.bafa_key.clone();
    let vtos_config = config.vtos.clone();
    tokio::spawn(async move {
        loop {
            match BafaClient::get_topic_list(api_key.clone()).await {
                Ok(topics) => {
                    for topic in topics.data.iter() {
                        if !topic.online {
                            let topic_name = topic.topic.clone();
                            let type_ = topic.type_.clone();
                            if let Err(e) =
                                BafaClient::delete_topic(api_key.clone(), topic_name, type_).await
                            {
                                log::warn!("无法删除巴法云 topic: {}", e);
                            } else {
                                log::info!("清理离线的巴法云 topic: {}", topic.name);
                            }
                        }
                    }
                    _ = BafaClient::create_topic(api_key.clone(), "有人呼叫", DEF_BAFA_CALL_NOTIFY)
                        .await
                }
                Err(err) => {
                    log::error!("无法初始化巴法云 {}", err);
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
            }
            // === 每次重连前，重新克隆一个 sender（client）或重新捕获 channel ===
            let mut options = MqttOptions::new(api_key.clone(), BAFA_MQTT_BROKER, 9501);
            options
                .set_credentials("def", "def")
                .set_keep_alive(Duration::from_secs(60))
                .set_clean_session(true);
            let (client, mut eventloop) = AsyncClient::new(options, 10);
            let mut topic_vto_map: HashMap<String, Arc<DahuaVTOClient>> = HashMap::new();
            for d in vtos_config.iter() {
                if !d.btn_name.is_empty() {
                    let topic_name = format!("mjbt{}006", d.id);
                    // 可选：确保 topic 存在
                    let _ = BafaClient::create_topic(api_key.clone(), &d.btn_name, &topic_name).await;
                    if let Err(e) = client.subscribe(&topic_name, QoS::AtLeastOnce).await {
                        log::warn!("订阅失败 {}: {}", topic_name, e);
                    }
                    if let Some(vto) = state.vto_clients.read().await.get(&d.id) {
                        topic_vto_map.insert(topic_name, vto.clone());
                    }
                }
            }
            let mut call_sub = tx.subscribe();
            if let Err(e) = client
                .subscribe(DEF_BAFA_CALL_NOTIFY, QoS::AtLeastOnce)
                .await
            {
                log::error!("{}订阅错误 {}", DEF_BAFA_CALL_NOTIFY, e);
            }
            log::info!("巴法云MQTT 已连接，主题已订阅，等待消息...");
            let client_clone = client.clone();
            loop {
                tokio::select! {
                    poll_result = eventloop.poll() => {
                        match poll_result {
                            Ok(Event::Incoming(Incoming::Publish(publish))) => {
                                // 处理消息的逻辑
                                let topic = publish.topic.clone();
                                match topic_vto_map.get(&topic) {
                                    Some(vto) => {
                                        if publish.payload=="on"{
                                            log::info!("{}收到巴法云开门消息 {} {:?}", vto.config.host, topic, String::from_utf8_lossy(&publish.payload).to_string());
                                            match state.clone().open_call_door().await {
                                                Ok(_) => {
                                                    log::info!("打开了呼叫的门");
                                                    let _ = client_clone.publish(topic, QoS::AtMostOnce, false, "off").await;
                                                    let _ = client_clone.publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "off").await;
                                                },
                                                Err(_) => {
                                                    if let Err(e) = vto.clone().open_door_and_cancel_call("").await {
                                                        log::error!("{}打开门失败 {}", vto.config.host, e);
                                                    }
                                                    let _ = client_clone.publish(topic, QoS::AtMostOnce, false, "off").await;
                                                }
                                            }
                                        }
                                    },
                                    None => {
                                        if topic != DEF_BAFA_CALL_NOTIFY {
                                            log::warn!("未找到{}对应的VTO设备", topic);                                    }
                                        }
                                }
                            }
                            Err(e) => {
                                log::error!("巴法云MQTT 错误: {}, 即将重连", e);
                                break; // 跳出内层 loop，触发外层重建连接
                            }
                            _ => {}
                        }
                    },
                    cc = call_sub.recv() => {
                        match cc {
                            Ok(vto) => {
                                log::info!("巴法云收到呼叫事件 {}", vto.config.host);
                                let _ = client_clone.publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "on").await
                                    .map_err(|e| log::warn!("发送巴法云mqtt消息错误 {}", e));
                                let client_clone = client.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    let _ = client_clone.publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "off").await
                                        .map_err(|e| log::warn!("发送巴法云mqtt消息错误 {}", e));
                                });
                            },
                            Err(e) => {
                                log::warn!("接收呼叫事件失败: {}", e);
                            }
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

impl BafaClient {
    pub async fn create_topic(api_key: String, name: &str, topic: &str) -> Result<()> {
        let url = format!("{}/v1/createTopic", BAFA_API_BASE);
        let payload = CreateTopicRequest {
            name: name.to_string(),
            uid: api_key.clone(),
            topic: topic.to_string(),
            type_: 1,
        };

        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_json(serde_json::to_value(payload)?)?;

        let res: BafaApiResponse2 = response.into_json()?;

        if res.code != 0 {
            return Err(anyhow!("API error: {}", res.message));
        }
        Ok(())
    }

    async fn get_topic_list(api_key: String) -> Result<ListTopicRow, anyhow::Error> {
        let url = format!(
            "https://apis.bemfa.com/vb/api/v2/allTopic?openID={}&type=1",
            api_key
        );

        let response = ureq::get(&url).call()?;
        let res: BafaApiResponse<ListTopicRow> = response.into_json()?;

        if res.code != 0 {
            return Err(anyhow!("API error: {}", res.msg));
        }

        res.data.ok_or_else(|| anyhow!("Missing topic list data"))
    }

    pub async fn delete_topic(api_key: String, topic: String, device_type: i32) -> Result<()> {
        let url = format!("{}/v1/deleteTopic", BAFA_API_BASE);
        let payload = DeleteTopicRequest {
            uid: api_key,
            topic,
            type_: device_type,
        };

        let response = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_json(serde_json::to_value(payload)?)?;

        let res: BafaApiResponse2 = response.into_json()?;

        if res.code != 0 {
            return Err(anyhow!("API error: {}", res.message));
        }
        Ok(())
    }
    // pub async fn get_topic_info(api_key: String, topic: String) -> Result<GetTopicRes> {
    //     let url = format!(
    //         "https://apis.bemfa.com/vb/api/v2/topicInfo?openID={}&type=1&topic={}",
    //         api_key, topic
    //     );
    //
    //     let response = ureq::get(&url).call()?;
    //     let res: BafaApiResponse<GetTopicRes> = response.into_json()?;
    //
    //     if res.code != 0 {
    //         return Err(anyhow!("API error: {}", res.msg));
    //     }
    //     res.data.ok_or_else(|| anyhow!("Missing topic info data"))
    // }
}
