//! Web admin / status console for the ESP32 firmware.
//!
//! Serves a small HTML dashboard at http://<sta-ip>/ and a JSON status
//! endpoint at /api/status over the STA interface so an operator can
//! check device health from a browser without a serial console.
//!
//! Runs on esp_http_server tasks; the owning thread only keeps the server
//! handle alive. Must be started only when lwIP is up.

use core::time::Duration;

use embedded_svc::http::Method;
use esp_idf_svc::http::server::{Configuration, EspHttpServer};

use crate::{WifiStatusHandle, free_heap, now_ms};
use magent_core::escape::{html as html_escape, json as json_escape};

pub fn run_web_admin(wifi_status: WifiStatusHandle) {
    let config = Configuration {
        stack_size: 8192,
        max_uri_handlers: 8,
        ..Default::default()
    };
    let mut server = match EspHttpServer::new(&config) {
        Ok(s) => s,
        Err(e) => {
            log::error!("[webadmin] EspHttpServer::new failed: {e}");
            return;
        }
    };

    let st = wifi_status.clone();
    if let Err(e) = server.fn_handler("/", Method::Get, move |mut req| -> Result<(), esp_idf_svc::sys::EspError> {
        let body = render_index(&st);
        let (_headers, conn) = req.split();
        conn.initiate_response(200, Some("OK"), &[("Content-Type", "text/html")])?;
        conn.write_all(body.as_bytes())?;
        Ok(())
    }) {
        log::warn!("[webadmin] register / handler failed: {e}");
    }

    let st = wifi_status.clone();
    if let Err(e) = server.fn_handler("/api/status", Method::Get, move |mut req| -> Result<(), esp_idf_svc::sys::EspError> {
        let body = render_status(&st);
        let (_headers, conn) = req.split();
        conn.initiate_response(200, Some("OK"), &[("Content-Type", "application/json")])?;
        conn.write_all(body.as_bytes())?;
        Ok(())
    }) {
        log::warn!("[webadmin] register /api/status handler failed: {e}");
    }

    log::info!("[webadmin] HTTP admin server listening on port 80");
    loop {
        std::thread::sleep(Duration::from_secs(30));
    }
}

fn render_index(wifi_status: &WifiStatusHandle) -> String {
    let s = wifi_status.lock().unwrap_or_else(|p| p.into_inner());
    let ip = html_escape(&s.ip);
    let ssid = html_escape(&s.ssid);
    format!("<!DOCTYPE html><html><head><title>mAgent v{0}</title></head><body><h1>mAgent v{0}</h1><table><tr><td>version</td><td>{0}</td></tr><tr><td>state</td><td>{1}</td></tr><tr><td>ip</td><td>{2}</td></tr><tr><td>ssid</td><td>{3}</td></tr><tr><td>rssi</td><td>{4} dBm</td></tr><tr><td>heap</td><td>{5} B</td></tr><tr><td>uptime</td><td>{6} ms</td></tr></table><p><a href=/api/status>JSON status</a></p></body></html>", env!("CARGO_PKG_VERSION"), s.state, ip, ssid, s.rssi, free_heap(), now_ms())
}

fn render_status(wifi_status: &WifiStatusHandle) -> String {
    let s = wifi_status.lock().unwrap_or_else(|p| p.into_inner());
    let ip = json_escape(&s.ip);
    let ssid = json_escape(&s.ssid);
    format!("{{\"version\":\"{}\",\"wifi_state\":{},\"ip\":\"{}\",\"ssid\":\"{}\",\"rssi_dbm\":{},\"free_heap_b\":{},\"uptime_ms\":{}}}", env!("CARGO_PKG_VERSION"), s.state, ip, ssid, s.rssi, free_heap(), now_ms())
}
