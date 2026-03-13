use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub signal: SignalConfig,
    pub video:  VideoConfig,
    pub audio:  AudioConfig,
    pub hid:    HidConfig,
    pub ice:    IceConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SignalConfig {
    pub url:     String,
    pub room_id: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VideoConfig {
    /// 视频源类型："v4l2"（USB 采集卡/摄像头）或 "screen"（本机屏幕）
    #[serde(default = "default_video_source")]
    pub source:       String,
    pub device:       PathBuf,
    pub width:        u32,
    pub height:       u32,
    pub fps:          u32,
    pub bitrate_kbps: u32,
    pub hw_encode:    bool,
}

fn default_video_source() -> String { "v4l2".to_string() }

#[derive(Debug, Deserialize, Clone)]
pub struct AudioConfig {
    pub device:       String,
    pub sample_rate:  u32,
    pub channels:     u32,
    pub bitrate_kbps: u32,
    pub enabled:      bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HidConfig {
    pub mode:            String,  // "gadget" | "ch9329"
    pub keyboard_device: PathBuf,
    pub mouse_device:    PathBuf,
    pub serial_port:     String,
    pub serial_baud:     u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IceConfig {
    pub stun_servers:  Vec<String>,
    pub turn_url:      Option<String>,
    pub turn_username: Option<String>,
    pub turn_password: Option<String>,
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read config file '{}': {}", path, e))?;
        let cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("config parse error: {}", e))?;
        Ok(cfg)
    }
}
