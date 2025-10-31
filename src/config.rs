// config.rs
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceConfig {
    pub id: String,
    pub ip: String,
    pub port: u16,
    #[serde(default)]
    pub userid: String,
    pub name: String,
    pub password: String,
    #[serde(default)]
    pub gpio: u16,
    #[serde(default)]
    pub btn_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub vtos: Vec<DeviceConfig>,
    pub listen: String,
    pub username: String,
    pub password: String,
    pub userid: String,
    #[serde(default)]
    pub gpio_out: String,
    #[serde(default)]
    pub bafa_key: String,
    #[serde(default)]
    pub auto_open: bool,
    #[serde(default)]
    pub sip_interface: String,
    #[serde(default)]
    pub mqtt_host: String,
    #[serde(default)]
    pub mqtt_user: String,
    #[serde(default)]
    pub mqtt_pass: String,
}

impl AppConfig {
    // 从文件读取配置
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Self::from_str(&contents)
    }

    // 从字符串解析配置
    pub fn from_str(contents: &str) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let mut config = AppConfig {
            vtos: Vec::new(),
            listen: "".to_string(),
            username: "".to_string(),
            password: "".to_string(),
            userid: "".to_string(),
            gpio_out: "".to_string(),
            bafa_key: "".to_string(),
            auto_open: false,
            sip_interface: "".to_string(),
            mqtt_host: "".to_string(),
            mqtt_user: "".to_string(),
            mqtt_pass: "".to_string(),
        };
        let mut current_section: Option<String> = None;
        let mut current_options: HashMap<String, String> = HashMap::new();

        for line in contents.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("config") {
                if let Some(section) = current_section.take() {
                    Self::parse_section(&section, &current_options, &mut config)?;
                }
                current_options.clear();
                current_section = Some(line.replace("config ", ""));
            } else if line.starts_with("option") {
                if let Some(parts) = line.splitn(3, ' ').collect::<Vec<&str>>().get(1..) {
                    if parts.len() == 2 {
                        let key = parts[0].to_string();
                        let value = parts[1].trim_matches('\'').to_string();
                        current_options.insert(key, value);
                    }
                }
            }
        }
        // 处理最后一个section
        if let Some(section) = current_section {
            Self::parse_section(&section, &current_options, &mut config)?;
        }
        Ok(Arc::new(config))
    }

    fn parse_section(
        section: &str,
        options: &HashMap<String, String>,
        config: &mut AppConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if section.starts_with("dahuavto_device") {
            let device_config = DeviceConfig {
                id: section.split_whitespace().nth(1).unwrap().to_string(),
                ip: options.get("ip").cloned().unwrap_or_default(),
                port: options
                    .get("port")
                    .cloned()
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(0),
                userid: options.get("userid").cloned().unwrap_or_default(),
                name: options.get("name").cloned().unwrap_or_default(),
                password: options.get("password").cloned().unwrap_or_default(),
                gpio: options
                    .get("gpio")
                    .cloned()
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(0),
                btn_name: options.get("btn_name").cloned().unwrap_or_default(),
            };
            config.vtos.push(device_config);
        } else if section == "dahuavto 'main'" {
            config.listen = options.get("listen").cloned().unwrap_or_default();
            config.username = options.get("username").cloned().unwrap_or_default();
            config.password = options.get("password").cloned().unwrap_or_default();
            config.userid = options.get("userid").cloned().unwrap_or_default();
            config.gpio_out = options.get("gpio_out").cloned().unwrap_or_default();
            config.bafa_key = options.get("bafa_key").cloned().unwrap_or_default();
            config.auto_open = options.get("auto_open").cloned().unwrap_or_default() == "true";
            config.sip_interface = options.get("sip_interface").cloned().unwrap_or_default();
            config.mqtt_host = options.get("mqtt_host").cloned().unwrap_or_default();
            config.mqtt_user = options.get("mqtt_user").cloned().unwrap_or_default();
            config.mqtt_pass = options.get("mqtt_pass").cloned().unwrap_or_default();
        }
        Ok(())
    }

    // 保存配置到文件
    pub fn to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), Box<dyn std::error::Error>> {
        let mut output = String::new();

        // 写入主配置
        output.push_str("config dahuavto 'main'\n");
        output.push_str(&format!("    option listen '{}'\n", self.listen));
        output.push_str(&format!("    option username '{}'\n", self.username));
        output.push_str(&format!("    option password '{}'\n", self.password));
        output.push_str(&format!("    option userid '{}'\n", self.userid));
        output.push_str(&format!("    option gpio_out '{}'\n", self.gpio_out));
        output.push_str(&format!("    option bafa_key '{}'\n", self.bafa_key));
        output.push_str(&format!("    option auto_open '{}'\n", self.auto_open));
        output.push_str(&format!(
            "    option sip_interface '{}'\n",
            self.sip_interface
        ));
        output.push_str(&format!("    option mqtt_host '{}'\n", self.mqtt_host));
        output.push_str(&format!("    option mqtt_user '{}'\n", self.mqtt_user));
        output.push_str(&format!("    option mqtt_pass '{}'\n", self.mqtt_pass));
        output.push_str("\n");

        // 写入设备配置
        for device in &self.vtos {
            output.push_str(&format!("config dahuavto_device {}\n", device.id));
            output.push_str(&format!("    option ip '{}'\n", device.ip));
            output.push_str(&format!("    option port '{}'\n", device.port));
            output.push_str(&format!("    option userid '{}'\n", device.userid));
            output.push_str(&format!("    option name '{}'\n", device.name));
            output.push_str(&format!("    option password '{}'\n", device.password));
            output.push_str(&format!("    option gpio '{}'\n", device.gpio));
            output.push_str(&format!("    option btn_name '{}'\n", device.btn_name));
            output.push_str("\n");
        }

        let mut file = File::create(path)?;
        file.write_all(output.as_bytes())?;

        Ok(())
    }
}
