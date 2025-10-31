use crate::dahuavto::{ClientError, DahuaVTOClient};
use pnet::datalink::{self, Channel};
use pnet::packet::ethernet::EthernetPacket;
use pnet::packet::ip::IpNextHeaderProtocols;
use pnet::packet::ipv4::Ipv4Packet;
use pnet::packet::udp::UdpPacket;
use pnet::packet::Packet;
use regex::Regex;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use std::sync::Arc;
use std::thread;
use tokio::sync::broadcast::Sender;
use tokio::sync::RwLock;

const INV: &'static [u8] = &['I' as u8, 'N' as u8, 'V' as u8]; // INVITE

#[derive(Debug)]
struct SipContact {
    ip_address: String,
    from_user: String,
}

pub struct SipServer {
    pub(crate) clients: Arc<HashMap<String, Arc<DahuaVTOClient>>>,
    pub(crate) clients_user: Arc<HashMap<String, Arc<DahuaVTOClient>>>,
    tx: Sender<Arc<DahuaVTOClient>>,
    interface_name: String,
}

impl SipServer {
    pub fn new(
        interface_name: String,
        tx: Sender<Arc<DahuaVTOClient>>,
        vto_clients: HashMap<String, Arc<DahuaVTOClient>>,
    ) -> Arc<Self> {
        let mut clients_user: HashMap<String, Arc<DahuaVTOClient>> = HashMap::new();
        vto_clients.iter().for_each(|(_, client)| {
            clients_user.insert(client.config.user_id.clone(), client.clone());
        });
        Arc::new(SipServer {
            interface_name,
            tx,
            clients: Arc::new(vto_clients),
            clients_user: Arc::new(clients_user),
        })
    }

    pub fn run(self: Arc<Self>) -> Result<(), Box<dyn Error>> {
        // 获取 br-lan 接口
        let interface_name = self.interface_name.clone();
        let interfaces = datalink::interfaces();
        let interface = interfaces
            .into_iter()
            .find(|iface| iface.name == interface_name)
            .ok_or(format!("找不到 {} 接口", interface_name))?;
        // 创建数据链路层捕获器
        let (_, mut rx) = match datalink::channel(&interface, Default::default()) {
            Ok(Channel::Ethernet(tx, rx)) => (tx, rx),
            Ok(_) => return Err("不支持的网卡接口".into()),
            Err(e) => return Err(e.into()),
        };
        thread::spawn(move || {
            log::info!("开始捕获 {:?} 上的数据包...", interface_name);
            loop {
                match rx.next() {
                    Ok(packet) => {
                        // 解析以太网帧
                        if let Some(ethernet) = EthernetPacket::new(packet) {
                            self.clone().handle_ethernet_frame(ethernet);
                        }
                    }
                    Err(e) => {
                        log::error!("捕获数据包时出错: {}", e);
                    }
                }
            }
        });
        Ok(())
    }

    pub fn handle_ethernet_frame(self: Arc<Self>, ethernet: EthernetPacket) {
        // 检查是否为 IPv4 包
        if ethernet.get_ethertype() != pnet::packet::ethernet::EtherTypes::Ipv4 {
            return;
        }
        if let Some(ipv4) = Ipv4Packet::new(ethernet.payload()) {
            if ipv4.get_next_level_protocol() != IpNextHeaderProtocols::Udp {
                return;
            } else {
                if let Some(udp) = UdpPacket::new(ipv4.payload()) {
                    if udp.get_destination() == 5060
                        || udp.get_source() == 5060
                        || udp.get_destination() == 5080
                        || udp.get_source() == 5080
                    {
                        // 提取 SIP 数据
                        let sip_payload = udp.payload();
                        match Self::parse_sip_message(sip_payload) {
                            Ok(res) => {
                                log::info!("SIP呼叫消息: {:?}", res);
                                if let Some(client) = self.clients.get(&res.ip_address) {
                                    if let Err(e) = self.tx.send(client.clone()) {
                                        log::error!("Failed to send client to tx: {}", e);
                                    };
                                } else if let Some(client) = self.clients_user.get(&res.from_user) {
                                    if let Err(e) = self.tx.send(client.clone()) {
                                        log::error!("Failed to send client to tx: {}", e);
                                    };
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }
    }

    fn parse_sip_message(payload: &[u8]) -> Result<(SipContact), Box<dyn Error>> {
        if payload.len() > 3 && &payload[0..3] == INV {
            log::info!("SIP呼叫报文: {:?}", String::from_utf8_lossy(payload));
            let message =
                std::str::from_utf8(payload).map_err(|e| Box::new(e) as Box<dyn Error>)?;
            let contact_regex = Regex::new(r"Contact:\s*<sip:[^@]*@([^:>]+)(?::\d+)?>")?;
            let ip_address = contact_regex
                .captures(message)
                .and_then(|cap| cap.get(1))
                .map(|m| m.as_str().to_string())
                .ok_or("无法从Contact字段解析IP地址")?;
            // 解析To字段中的用户名
            let from_regex = Regex::new(r"From:\s*<sip:([^@]+)@")?;
            let from_user = from_regex
                .captures(message)
                .and_then(|cap| cap.get(1))
                .map(|m| m.as_str().to_string())
                .ok_or("无法从To字段解析用户名")?;
            return Ok(SipContact {
                ip_address,
                from_user,
            });
        }
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Not a SIP message",
        )))
    }
}
