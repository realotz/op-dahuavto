use std::io::SeekFrom;
use std::path::Path;
use std::time::Duration;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;
use tokio::time::sleep;

const GPIO_BASE: &str = "/sys/class/gpio";
const DEF_GPIO_OUT: &str = "call_notify";
pub async fn call_notify(gpio: &str) -> io::Result<()> {
    if gpio != DEF_GPIO_OUT {
        // 设置高电平
        fs::write(
            Path::new(GPIO_BASE)
                .join(format!("gpio{}", gpio))
                .join("value"),
            "1",
        )
        .await?;
        sleep(Duration::from_millis(120)).await;
        // 设置低电平
        fs::write(
            Path::new(GPIO_BASE)
                .join(format!("gpio{}", gpio))
                .join("value"),
            "0",
        )
        .await?;
    } else {
        // 设置低电平
        fs::write(Path::new(GPIO_BASE).join(gpio).join("value"), "0").await?;
        sleep(Duration::from_millis(120)).await;
        // 设置高电平
        fs::write(Path::new(GPIO_BASE).join(gpio).join("value"), "1").await?;
    }
    Ok(())
}

// 初始化GPIO（导出+方向设置）
pub async fn init_gpio(gpio: &str, is_out: bool) -> io::Result<()> {
    if gpio == DEF_GPIO_OUT || gpio == "" {
        return Ok(());
    }
    let gpio_str = format!("gpio{}", gpio);
    let gpio_path = Path::new(GPIO_BASE).join(&gpio_str);

    // 检查是否已导出
    if !gpio_path.exists() {
        // 导出GPIO
        fs::write(Path::new(GPIO_BASE).join("export"), gpio).await?;

        sleep(Duration::from_millis(100)).await; // 等待系统创建目录
    }

    // 设置方向
    let direction = if is_out { "out" } else { "in" };
    fs::write(gpio_path.join("direction"), direction).await?;

    log::info!("GPIO初始化成功 :{}", gpio);
    Ok(())
}

// 监听GPIO状态变化

pub async fn watch_gpio(gpio_num: u16) -> io::Result<mpsc::Receiver<i32>> {
    let value_file = Path::new(GPIO_BASE)
        .join(format!("gpio{}", gpio_num))
        .join("value");
    let (tx, rx) = mpsc::channel(1);
    let mut file = fs::File::open(&value_file).await?;
    tokio::spawn(async move {
        let mut last_value = 1;
        loop {
            let current_value = match read_value(&mut file).await {
                Ok(value) => value,
                Err(_) => {
                    sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            if current_value != last_value {
                if current_value == 0 {
                    log::info!("GPIO状态变化: {} -> {}", last_value, current_value);
                    let _ = tx.send(current_value).await;
                }
                last_value = current_value;
            }
            sleep(Duration::from_millis(100)).await;
        }
    });
    Ok(rx)
}

// 读取当前GPIO值
async fn read_value(file: &mut fs::File) -> io::Result<i32> {
    let mut buf = [0u8; 1];
    file.seek(SeekFrom::Start(0)).await?;
    file.read_exact(&mut buf).await?;
    Ok(if buf[0] == b'0' { 0 } else { 1 })
}
