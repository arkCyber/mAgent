//! WiFi自动重连管理器 - 生产级实现
//!
//! 提供完整的WiFi连接生命周期管理：
//! - 初始连接和配置
//! - 断线检测和自动重连
//! - 指数退避算法
//! - 信号质量监控
//! - 连接状态发布
//!
//! 集成到ESP32-S3/C61固件的WiFi supervisor线程。

use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use esp_idf_sys as sys;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// WiFi连接质量评级
///
/// `Unknown` is the failure case: the RSSI query did not succeed (e.g. the STA
/// is not associated), so no quality can be claimed. It must stay distinct from
/// every real grade — a failed read previously collapsed to `rssi == 0`, which
/// `from_rssi` graded as `Excellent` (the *best* possible signal) and thereby
/// suppressed the weak-signal warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionQuality {
    Unknown,    // RSSI 读取失败
    Excellent,  // RSSI > -50 dBm
    Good,       // RSSI > -60 dBm
    Fair,       // RSSI > -70 dBm
    Poor,       // RSSI > -80 dBm
    VeryPoor,   // RSSI <= -80 dBm
}

impl ConnectionQuality {
    /// 从RSSI计算连接质量
    pub fn from_rssi(rssi: i32) -> Self {
        match rssi {
            r if r > -50 => Self::Excellent,
            r if r > -60 => Self::Good,
            r if r > -70 => Self::Fair,
            r if r > -80 => Self::Poor,
            _ => Self::VeryPoor,
        }
    }

    /// 是否应该告警（质量差）
    pub fn should_warn(&self) -> bool {
        matches!(self, Self::Poor | Self::VeryPoor)
    }
}

/// WiFi重连状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectState {
    /// 初始状态，从未连接
    Initial,
    /// 已连接，运行正常
    Connected,
    /// 检测到断线，准备重连
    Disconnected,
    /// 正在重连中
    Reconnecting,
    /// 达到最大重试次数，进入故障模式
    Failed,
}

/// WiFi管理器配置
///
/// Deliberately holds no credentials. The manager only *reacts* to a radio
/// that `main` has already configured (see the supervisor's initial-associate
/// path); `reconnect` re-issues `wifi.connect()` against whatever the netif is
/// already configured with. Carrying an SSID/password here would leave a copy
/// of the WPA2 passphrase resident in the manager for no functional gain.
pub struct WifiManagerConfig {
    /// 最大重试次数（0 = 无限重试）
    pub max_retries: u32,
    /// 初始退避时间
    pub initial_backoff: Duration,
    /// 最大退避时间
    pub max_backoff: Duration,
}

impl Default for WifiManagerConfig {
    fn default() -> Self {
        Self {
            max_retries: 0,  // 无限重试
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
        }
    }
}

/// WiFi管理器内部状态
struct WifiManagerState {
    /// 重连状态
    state: ReconnectState,
    /// 当前重试计数
    retry_count: u32,
    /// 当前退避时间
    current_backoff: Duration,
    /// 上次断线时间
    last_disconnect: Option<Instant>,
    /// 上次重连尝试时间
    last_reconnect_attempt: Option<Instant>,
    /// 累计成功连接次数
    total_connects: u64,
    /// 累计断线次数
    total_disconnects: u64,
}

impl WifiManagerState {
    fn new() -> Self {
        Self {
            state: ReconnectState::Initial,
            retry_count: 0,
            current_backoff: Duration::from_secs(2),
            last_disconnect: None,
            last_reconnect_attempt: None,
            total_connects: 0,
            total_disconnects: 0,
        }
    }

    /// 记录成功连接
    fn mark_connected(&mut self) {
        self.state = ReconnectState::Connected;
        self.retry_count = 0;
        self.current_backoff = Duration::from_secs(2);
        self.total_connects += 1;
    }

    /// 记录断线
    fn mark_disconnected(&mut self) {
        self.state = ReconnectState::Disconnected;
        self.last_disconnect = Some(Instant::now());
        self.total_disconnects += 1;
    }

    /// 计算下次重连的退避时间（指数退避）
    fn calculate_backoff(&mut self, config: &WifiManagerConfig) -> Duration {
        // 指数退避: initial * 2^retry_count, 上限为max_backoff
        let backoff = config.initial_backoff * (1 << self.retry_count.min(5));
        self.current_backoff = backoff.min(config.max_backoff);
        self.current_backoff
    }

    /// 是否应该尝试重连
    fn should_reconnect(&self) -> bool {
        match self.state {
            ReconnectState::Disconnected => {
                // 立即尝试第一次重连
                true
            }
            ReconnectState::Reconnecting => {
                // 检查是否过了退避时间
                if let Some(last_attempt) = self.last_reconnect_attempt {
                    last_attempt.elapsed() >= self.current_backoff
                } else {
                    true
                }
            }
            ReconnectState::Failed => {
                // 故障模式下不自动重连（需要人工介入）
                false
            }
            _ => false,
        }
    }

    /// 记录重连尝试
    fn mark_reconnect_attempt(&mut self, config: &WifiManagerConfig) {
        self.state = ReconnectState::Reconnecting;
        self.retry_count += 1;
        self.last_reconnect_attempt = Some(Instant::now());

        // 检查是否达到最大重试次数
        if config.max_retries > 0 && self.retry_count >= config.max_retries {
            self.state = ReconnectState::Failed;
            log::error!(
                "[wifi] 达到最大重试次数 ({}), 进入故障模式",
                config.max_retries
            );
        }
    }
}

/// WiFi自动重连管理器
pub struct WifiManager {
    config: WifiManagerConfig,
    state: Arc<Mutex<WifiManagerState>>,
}

impl WifiManager {
    /// 创建新的WiFi管理器
    pub fn new(config: WifiManagerConfig) -> Self {
        log::info!("[wifi-mgr] 初始化 WiFi 管理器");
        Self {
            config,
            state: Arc::new(Mutex::new(WifiManagerState::new())),
        }
    }

    /// 尝试重连
    pub fn reconnect(
        &self,
        wifi: &mut BlockingWifi<EspWifi<'_>>,
    ) -> Result<(), esp_idf_sys::EspError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // 检查是否应该重连
        if !state.should_reconnect() {
            let backoff = state.current_backoff;
            drop(state);
            log::debug!("[wifi-mgr] 等待退避时间: {:?}", backoff);
            return Err(esp_idf_sys::EspError::from_infallible::<
                { esp_idf_sys::ESP_ERR_INVALID_STATE },
            >());
        }

        // 记录重连尝试
        state.mark_reconnect_attempt(&self.config);
        let retry_count = state.retry_count;
        let backoff = state.calculate_backoff(&self.config);
        drop(state);

        log::info!(
            "[wifi-mgr] 🔄 重连尝试 #{}, 下次退避: {:?}",
            retry_count,
            backoff
        );

        // 尝试重连
        match wifi.connect() {
            Ok(_) => {
                // 等待IP地址
                let timeout = Instant::now() + Duration::from_secs(15);
                while Instant::now() < timeout {
                    if wifi.is_connected()? && self.have_ip(wifi).is_some() {
                        self.state.lock().unwrap_or_else(|e| e.into_inner()).mark_connected();
                        log::info!("[wifi-mgr] ✅ 重连成功！");
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }

                log::warn!("[wifi-mgr] 重连超时");
                Err(esp_idf_sys::EspError::from_infallible::<
                    { esp_idf_sys::ESP_ERR_TIMEOUT },
                >())
            }
            Err(e) => {
                log::warn!("[wifi-mgr] 重连失败: {:?}", e);
                Err(e)
            }
        }
    }

    /// 检查连接状态并自动重连
    pub fn check_and_reconnect(
        &self,
        wifi: &mut BlockingWifi<EspWifi<'_>>,
    ) -> Result<bool, esp_idf_sys::EspError> {
        let is_connected = wifi.is_connected()?;
        let has_ip = self.have_ip(wifi).is_some();

        if !is_connected || !has_ip {
            // 检测到断线
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.state == ReconnectState::Connected {
                state.mark_disconnected();
                drop(state);
                log::warn!("[wifi-mgr] ⚠️ 检测到断线，准备重连...");
            } else {
                drop(state);
            }

            // 尝试重连
            self.reconnect(wifi)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 获取信号强度（RSSI）。失败返回 `None`（例如 STA 未关联）。
    ///
    /// `esp_wifi_sta_get_rssi` takes a `*mut c_int` (i32) — not an `i8`, even
    /// though the value it writes back is a dBm reading that fits in one byte.
    /// Writing through an `i8` pointer here would have been a 4-byte store into
    /// a 1-byte local.
    pub fn get_rssi(&self, _wifi: &mut BlockingWifi<EspWifi<'_>>) -> Option<i32> {
        unsafe {
            let mut rssi_val: i32 = 0;
            (sys::esp_wifi_sta_get_rssi(&mut rssi_val) == 0).then_some(rssi_val)
        }
    }

    /// 获取IP地址
    fn have_ip(&self, wifi: &BlockingWifi<EspWifi<'_>>) -> Option<String> {
        wifi.wifi()
            .sta_netif()
            .get_ip_info()
            .ok()
            .and_then(|info| {
                let ip = info.ip;
                if ip.octets() != [0, 0, 0, 0] {
                    Some(format!("{}.{}.{}.{}", ip.octets()[0], ip.octets()[1], ip.octets()[2], ip.octets()[3]))
                } else {
                    None
                }
            })
    }

    /// 获取当前状态（供日志/监控/`AT+CWSTATE` 使用）
    pub fn get_state_summary(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        format!(
            "state={:?}, retries={}, connects={}, disconnects={}",
            state.state, state.retry_count, state.total_connects, state.total_disconnects
        )
    }

    /// 重置故障状态（人工恢复）
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        log::info!("[wifi-mgr] 重置管理器状态");
        state.state = ReconnectState::Initial;
        state.retry_count = 0;
        state.current_backoff = self.config.initial_backoff;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_quality() {
        assert_eq!(ConnectionQuality::from_rssi(-45), ConnectionQuality::Excellent);
        assert_eq!(ConnectionQuality::from_rssi(-55), ConnectionQuality::Good);
        assert_eq!(ConnectionQuality::from_rssi(-65), ConnectionQuality::Fair);
        assert_eq!(ConnectionQuality::from_rssi(-75), ConnectionQuality::Poor);
        assert_eq!(ConnectionQuality::from_rssi(-85), ConnectionQuality::VeryPoor);
    }

    #[test]
    fn test_unknown_quality_is_not_a_grade() {
        // A failed RSSI read must not be graded as a real signal level, and
        // must not suppress the weak-signal warning by claiming "Excellent".
        assert_ne!(ConnectionQuality::Unknown, ConnectionQuality::Excellent);
        assert!(!ConnectionQuality::Unknown.should_warn());
        assert!(ConnectionQuality::Poor.should_warn());
        assert!(ConnectionQuality::VeryPoor.should_warn());
        assert!(!ConnectionQuality::Excellent.should_warn());
    }

    #[test]
    fn test_backoff_calculation() {
        let config = WifiManagerConfig {
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            ..Default::default()
        };
        let mut state = WifiManagerState::new();

        // 第一次: 2s
        state.retry_count = 0;
        assert_eq!(state.calculate_backoff(&config), Duration::from_secs(2));

        // 第二次: 4s
        state.retry_count = 1;
        assert_eq!(state.calculate_backoff(&config), Duration::from_secs(4));

        // 第三次: 8s
        state.retry_count = 2;
        assert_eq!(state.calculate_backoff(&config), Duration::from_secs(8));

        // 第七次及以上: 上限60s
        state.retry_count = 6;
        assert_eq!(state.calculate_backoff(&config), Duration::from_secs(60));
    }

    #[test]
    fn test_state_transitions() {
        let config = WifiManagerConfig::default();
        let mut state = WifiManagerState::new();

        assert_eq!(state.state, ReconnectState::Initial);

        state.mark_connected();
        assert_eq!(state.state, ReconnectState::Connected);
        assert_eq!(state.retry_count, 0);
        assert_eq!(state.total_connects, 1);

        state.mark_disconnected();
        assert_eq!(state.state, ReconnectState::Disconnected);
        assert_eq!(state.total_disconnects, 1);

        state.mark_reconnect_attempt(&config);
        assert_eq!(state.state, ReconnectState::Reconnecting);
        assert_eq!(state.retry_count, 1);
    }
}
