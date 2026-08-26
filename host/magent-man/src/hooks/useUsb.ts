import { invoke } from '@tauri-apps/api/core';

// ---------------------------------------------------------------------------
// USB-serial transport hooks — talk to the C61 over its UART0 console port
// instead of BLE (the firmware runs with the `ble` feature disabled).
// ---------------------------------------------------------------------------

/** List USB-serial ports that look like the C61 bridge. */
export async function usbListPorts(): Promise<string[]> {
  try {
    return await invoke<string[]>('usb_list_ports');
  } catch (error) {
    console.error('USB list ports failed:', error);
    return [];
  }
}

/** Open a USB-serial port (e.g. `/dev/cu.usbserial-10`). */
export async function usbOpen(path: string): Promise<{ success: boolean; path: string }> {
  try {
    return await invoke('usb_open', { path });
  } catch (error) {
    console.error('USB open failed:', error);
    throw new Error(`USB open failed: ${error}`);
  }
}

/** Close the USB-serial port. */
export async function usbClose(): Promise<{ success: boolean }> {
  try {
    return await invoke('usb_close');
  } catch (error) {
    console.error('USB close failed:', error);
    throw new Error(`USB close failed: ${error}`);
  }
}

/** Send a raw AT command over USB and return the device response. */
export async function usbSendAt(cmd: string): Promise<{ success: boolean; response: string }> {
  try {
    return await invoke('usb_send_at', { cmd });
  } catch (error) {
    console.error('USB send AT failed:', error);
    throw new Error(`USB AT failed: ${error}`);
  }
}

/** Chat with the on-device agent over USB (`AT+AGENT="..."`). */
export async function usbAgentChat(message: string): Promise<{ success: boolean; response: string; timestamp?: string }> {
  try {
    return await invoke('usb_agent_chat', { message });
  } catch (error) {
    console.error('USB agent chat failed:', error);
    throw new Error(`USB agent chat failed: ${error}`);
  }
}

/** Report the current USB-serial connection state. */
export async function usbGetStatus(): Promise<{ connected: boolean; path: string | null }> {
  try {
    return await invoke('usb_get_status');
  } catch (error) {
    console.error('USB get status failed:', error);
    return { connected: false, path: null };
  }
}

export interface UsbDeviceInfo {
  wifi?: string;
  ip?: string;
  heap?: string;
  llm?: string;
  version?: string;
  uptime?: string;
}

/**
 * Fetch key device info over USB by issuing a batch of AT queries.
 * Returns a structured snapshot for the UI.
 */
export async function usbDeviceInfo(): Promise<UsbDeviceInfo> {
  const info: UsbDeviceInfo = {};
  try {
    const cw = await usbSendAt('AT+CWSTATE?');
    const cwTxt = cw.response.trim();
    const m = cwTxt.match(/\+CWSTATE:(\d+)(?:,([0-9.]+))?/);
    if (m) {
      const state = Number(m[1]);
      info.wifi = state >= 5 ? '已连接' : state === 0 ? '未连接' : '连接中';
      info.ip = m[2] || undefined;
    }
  } catch { /* ignore */ }
  try {
    const h = await usbSendAt('AT+HEAP');
    const m = h.response.trim().match(/(\d+)/);
    if (m) info.heap = `${(Number(m[1]) / 1024).toFixed(0)} KB`;
  } catch { /* ignore */ }
  try {
    const l = await usbSendAt('AT+LLMCFG?');
    // Stop at a comma or any whitespace so the trailing "OK" is not captured.
    const m = l.response.trim().match(/\+LLMCFG:([^,\s]+)/);
    if (m) info.llm = m[1];
  } catch { /* ignore */ }
  try {
    const v = await usbSendAt('AT+GMR');
    // Exclude CR/LF control characters (not escaped backslashes) so the
    // trailing "OK" is not captured.
    const m = v.response.trim().match(/\+GMR:([^\r\n]+)/);
    if (m) info.version = m[1].trim();
  } catch { /* ignore */ }
  try {
    const u = await usbSendAt('AT+UPTIME?');
    const m = u.response.trim().match(/(\d+)/);
    if (m) info.uptime = `${(Number(m[1]) / 1000).toFixed(0)}s`;
  } catch { /* ignore */ }
  return info;
}

