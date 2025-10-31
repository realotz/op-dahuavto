use crate::config::{AppConfig, DeviceConfig};
use crate::dahuavto::DahuaVTOClient;
use crate::http_server::ServerState;
use rumqttc::tokio_rustls::rustls::internal::msgs::handshake::ClientExtension;
use rumqttc::{AsyncClient, ClientError, Event, Incoming, MqttOptions, QoS};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::Sender;
use tokio::time::sleep;

const DEF_BAFA_CALL_NOTIFY: &str = "dahuavto/state";

pub async fn init_start_mqtt(
    config: Arc<AppConfig>,
    state: ServerState,
    tx: Sender<Arc<DahuaVTOClient>>,
) -> anyhow::Result<(), ClientError> {
    if config.mqtt_host.is_empty() {
        return Ok(());
    }
    let mqtt_host = config.mqtt_host.clone();
    let mqtt_user = config.mqtt_user.clone();
    let mqtt_pass = config.mqtt_pass.clone();
    let vtos_config = config.vtos.clone();
    // 解析 MQTT 主机地址和端口
    let (host, port) = if let Some((host_part, port_part)) = mqtt_host.clone().split_once(':') {
        (host_part.to_string(), port_part.parse().unwrap_or(1883))
    } else {
        (mqtt_host, 1883)
    };
    let host_id = gethostname::gethostname().to_string_lossy().to_string();
    tokio::spawn(async move {
        loop {
            let mut options = MqttOptions::new(host_id.clone(), host.clone(), port);
            options
                .set_credentials(mqtt_user.clone(), mqtt_pass.clone())
                .set_keep_alive(Duration::from_secs(60))
                .set_clean_session(true);
            let (client, mut eventloop) = AsyncClient::new(options, 100);
            let mut topic_vto_map: HashMap<String, Arc<DahuaVTOClient>> = HashMap::new();
            for d in vtos_config.iter() {
                if !d.btn_name.is_empty() {
                    let topic_name = format!("dahuavto/open_door_{}/set", d.id);
                    if let Some(vto) = state.vto_clients.read().await.get(&d.id) {
                        topic_vto_map.insert(topic_name, vto.clone());
                    }
                }
            }
            if let Err(e) = configure_ha_discovery(client.clone(), vtos_config.clone()).await {
                log::error!("{}", e);
            }
            let mut call_sub = tx.subscribe();
            log::info!("MQTT 已连接，主题已订阅，等待消息...");
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
                                        log::info!("{}收到MQTT开门消息 {} {:?}", vto.config.host, topic, String::from_utf8_lossy(&publish.payload).to_string());
                                            match state.clone().open_call_door().await {
                                                Ok(_) => {
                                                    log::info!("打开了呼叫的门");
                                                    let _ = client_clone.publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "OFF").await;
                                                },
                                                Err(_) => {
                                                    if let Err(e) = vto.clone().open_door_and_cancel_call("").await {
                                                        log::error!("{}打开门失败 {}", vto.config.host, e);
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
                                log::error!("MQTT 错误: {}, 即将重连", e);
                                sleep(Duration::from_secs(10)).await;
                                break;
                            }
                            _ => {}
                        }
                    },
                    cc = call_sub.recv() => {
                        match cc {
                            Ok(vto) => {
                                log::info!("MQTT收到呼叫事件 {}", vto.config.host);
                                let _ = client_clone.publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "ON").await
                                    .map_err(|e| log::warn!("发送mqtt消息错误 {}", e));
                                let client_clone = client.clone();
                                tokio::spawn(async move {
                                    tokio::time::sleep(Duration::from_secs(20)).await;
                                    let _ = client_clone.publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "OFF").await
                                        .map_err(|e| log::warn!("发送mqtt消息错误 {}", e));
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

async fn configure_ha_discovery(
    client: AsyncClient,
    vtos: Vec<DeviceConfig>,
) -> Result<(), ClientError> {
    let discovery_prefix = "homeassistant";
    // 配置门禁呼叫传感器（二进制传感器）
    let doorbell_sensor_config = json!({
        "name": "门禁呼叫",
        "unique_id": "rust_dahuavto_sensor",
        "device_class": "occupancy",
        "state_topic": DEF_BAFA_CALL_NOTIFY,
        "payload_on": "ON",
        "payload_off": "OFF",
        "device": {
            "name": "大华门禁系统",
            "identifiers": ["rust_door_system"],
            "manufacturer": "Rust Dahuavto HomeAssistant",
            "model": "v1.0"
        }
    });
    // 发布自动发现配置
    client
        .publish(
            format!("{}/binary_sensor/doorbell/config", discovery_prefix),
            QoS::AtLeastOnce,
            false,
            serde_json::to_string(&doorbell_sensor_config).unwrap(),
        )
        .await?;
    let _ = client
        .publish(DEF_BAFA_CALL_NOTIFY, QoS::AtMostOnce, false, "OFF")
        .await
        .map_err(|e| log::warn!("发送mqtt消息错误 {}", e));
    for vto in vtos {
        if !vto.btn_name.is_empty() {
            // // 配置摄像头实体
            // let camera_config = json!({
            //     "name": format!("{}监控", vto.btn_name),
            //     "unique_id": format!("dahuavto_camera_{}", vto.id),
            //     "device": {
            //         "name": "大华门禁系统",
            //         "identifiers": ["rust_door_system"],
            //         "manufacturer": "Rust Dahuavto HomeAssistant",
            //         "model": "v1.0"
            //     },
            //     "rtsp_transport":"tcp",
            //     "verify_ssl":false,
            //     "stream_source": format!("rtsp://{}:{}@{}:554/cam/realmonitor?channel=1&subtype=1",
            //         vto.name, vto.password, vto.ip)
            // });
            // log::info!("rtsp://{}:{}@{}:554/cam/realmonitor?channel=1&subtype=1",
            //         vto.name, vto.password, vto.ip);
            // log::info!("http://{}:{}@{}/cgi-bin/snapshot.cgi?channel=1",
            //         vto.name, vto.password, vto.ip);
            // // 发布摄像头自动发现配置
            // client
            //     .publish(
            //         format!("{}/camera/dahuavto_camera_{}/config", discovery_prefix, vto.id),
            //         QoS::AtLeastOnce,
            //         false,
            //         serde_json::to_string(&camera_config).unwrap(),
            //     )
            //     .await?;
            let topic_name = format!("dahuavto/open_door_{}/set", vto.id);
            let open_button_config = json!({
                 "name": vto.btn_name,
                 "unique_id": topic_name,
                  "command_topic": topic_name,
                  "payload_press": "OPEN",
                "device": {
                    "name": "大华门禁系统",
                    "identifiers": ["rust_door_system"],
                    "manufacturer": "Rust Dahuavto HomeAssistant",
                    "model": "v1.0"
                }
            });
            client.publish(
                    format!("{}/button/open_door_{}/config", discovery_prefix, vto.id),
                    QoS::AtLeastOnce,
                    false,
                    serde_json::to_string(&open_button_config).unwrap(),
                )
                .await?;
            // "stream_source": format!("rtsp://{}:{}@{}:554/cam/realmonitor?channel=1&subtype=1",vto.name,vto.password,vto.ip),
            // "still_image_url": format!("http://{}:{}@{}/cgi-bin/snapshot.cgi?channel=1", vto.name,vto.password,vto.ip),
            if let Err(e) = client.subscribe(&topic_name, QoS::AtLeastOnce).await {
                log::warn!("订阅失败 {}: {}", topic_name, e);
            }
        }
    }
    log::info!("已向HomeAssistant发送自动发现配置");
    Ok(())
}
