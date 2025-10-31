# Dahuavto-rs 项目文档

## 项目概述
Dahuavto-rs 是一个基于 Rust 语言开发的基于OpenWrt的ha、米家接入插件
#### 详细文档
大华vto插件折腾指南 https://www.yuque.com/u26879/ofk0rg/bvnbtvmgp6g8v4ge#J9arr 

dahua门禁接入方案详细参考 https://www.yuque.com/u26879/ofk0rg/wm60zf4td46lidbt
## 主要功能

### 核心功能
- **大华VTO设备连接**：通过TCP协议与大华门禁设备建立连接并保持心跳
- **SIP呼叫监听**：通过网络抓包技术监听SIP协议呼叫消息
- **远程开门控制**：支持通过Web界面、API调用和GPIO触发开门
- **多设备管理**：支持同时管理多个VTO设备

### 集成功能
- **HomeAssistant MQTT集成**：自动发现并创建HA门禁，监控需要处理网络IP转发自行接入webrtc
- **巴法云平台支持**：通过巴法云实现远程控制和状态同步
- **GPIO控制**：可配置GPIO与米家干接点模块接入米家
- **SIP监听**：部分设备需要通过配置SIP监听来获取呼叫状态，VTO下的用户ID需要填入门口机ID （非SIP下填自己家，默认全局）

### 管理功能
- **Web配置界面**：提供基础的Web界面进行系统配置
- **自动重连**：设备断开连接后自动重连机制

## 系统架构

### 主要模块
1. **main.rs** - 程序入口和主循环
2. **config.rs** - 配置管理和持久化
3. **dahuavto.rs** - 大华VTO设备通信协议实现
4. **sip.rs** - SIP协议监听和解析
5. **gpio.rs** - GPIO设备控制
6. **http_server.rs** - Web服务器和API接口
7. **mqtt.rs** - HomeAssistant MQTT集成
8. **bafa_cloud.rs** - 巴法云平台集成

## 安装和部署

### 环境要求
- OpenWrt系统（支持musl目标环境）
- Rust工具链（用于编译）
- 网络接口支持（用于SIP监听）

### 编译安装
自行参考openwrt插件编译

### 配置文件
配置文件位于 `/etc/config/dahuavto`，采用UCI格式：

```conf
config dahuavto 'main'
    option listen ':2066'
    option username 'admin'
    option password 'real123'
    option userid '201'
    option gpio_out 'call_notify'
    option bafa_key ''
    option auto_open 'false'
    option sip_interface 'eth0'
    option mqtt_host ''
    option mqtt_user ''
    option mqtt_pass ''

config dahuavto_device 1
    option ip '192.168.13.1'
    option port '80'
    option userid ''
    option name 'admin'
    option password 'admin123'
    option gpio '516'
    option btn_name '一楼'
```

## 使用说明

### Web管理界面
访问 `http://<设备IP>:2066` 进入Web管理界面，默认用户名密码为 admin/real123

### API接口
- `GET /api/open?id=<设备ID>` - 远程开门
- `GET /api/test` - 测试GPIO功能
- `GET /api/config` - 获取配置
- `POST /api/config` - 更新配置

### GPIO配置
- **呼叫提醒GPIO**：配置`gpio_out`选项，有人呼叫时触发
- **开门按钮GPIO**：在设备配置中设置`gpio`选项，按下时开门

## 集成指南

### HomeAssistant集成
1. 配置MQTT服务器信息
2. 系统会自动创建以下实体：
   - 二进制传感器：门禁呼叫状态
   - 按钮：每个设备的开门按钮
   - 摄像头：设备视频流（需手动配置）

### 巴法云集成
1. 在配置中设置巴法云密钥
2. 系统会自动创建和管理巴法云主题

### SIP监听配置
设置`sip_interface`为要监听的网络接口名称，如`eth0`

## 故障排除

### 常见问题
1. **设备连接失败**：检查IP地址、端口、用户名和密码是否正确
2. **SIP监听无效**：确认网络接口名称正确且有SIP流量
3. **GPIO不工作**：确认GPIO编号正确且权限足够

### 日志查看
```bash
# 查看系统日志
logread

# 查看应用日志
tail -f /var/log/messages | grep dahuavto
```

## 开发说明

### 项目结构
```
op-dahuavto/
├── src/
│   ├── main.rs          # 程序入口
│   ├── config.rs        # 配置管理
│   ├── dahuavto.rs      # VTO协议实现
│   ├── sip.rs           # SIP监听
│   ├── gpio.rs          # GPIO控制
│   ├── http_server.rs   # Web服务器
│   ├── mqtt.rs          # MQTT集成
│   └── bafa_cloud.rs    # 巴法云集成
├── files/
│   ├── dahuavto.conf    # 示例配置
│   └── dahuavto-rs.init # init脚本
└── Cargo.toml          # 项目配置
```

## 许可证

本项目采用 MIT OR Apache-2.0 双许可证协议。
