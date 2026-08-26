import { invoke } from '@tauri-apps/api/core';
import type { BleDevice, DeviceConfig, BleResult } from '../types';

// Type definitions for BLE responses
export interface SystemStatusResponse {
  state: number;
  wifi_state: number;
  memory_free: number;
  uptime_ms: number;
  error_code: number;
}

export interface DeviceInfoResponse {
  version_major: number;
  version_minor: number;
  version_patch: number;
  memory_total: number;
  memory_free: number;
  uptime_ms: number;
  chip_model: string;
}

export interface WifiStatusResponse {
  state: number;
  rssi: number;
  ip_addr: string;
  ssid: string;
}

export interface ConversationEntry {
  timestamp: number;
  role: 'user' | 'assistant' | 'system';
  text: string;
}

export interface ConversationsResponse {
  conversations: ConversationEntry[];
}

/**
 * Scan for nearby BLE devices
 */
export async function bleScan(): Promise<BleDevice[]> {
  try {
    const result = await invoke<BleDevice[]>('ble_scan');
    return result;
  } catch (error) {
    console.error('BLE scan failed:', error);
    throw new Error(`Scan failed: ${error}`);
  }
}

/**
 * Connect to a BLE device
 */
export async function bleConnect(deviceId: string): Promise<BleResult> {
  try {
    const result = await invoke<BleResult>('ble_connect', { deviceId });
    return result;
  } catch (error) {
    console.error('BLE connect failed:', error);
    throw new Error(`Connection failed: ${error}`);
  }
}

/**
 * Disconnect from current BLE device
 */
export async function bleDisconnect(): Promise<BleResult> {
  try {
    const result = await invoke<BleResult>('ble_disconnect');
    return result;
  } catch (error) {
    console.error('BLE disconnect failed:', error);
    throw new Error(`Disconnect failed: ${error}`);
  }
}

/**
 * Read current configuration from connected device
 */
export async function bleReadConfig(): Promise<DeviceConfig> {
  try {
    const result = await invoke<DeviceConfig>('ble_read_config');
    return result;
  } catch (error) {
    console.error('Read config failed:', error);
    throw new Error(`Failed to read config: ${error}`);
  }
}

/**
 * Write WiFi configuration to device
 */
export async function bleWriteWifi(ssid: string, password: string): Promise<BleResult> {
  if (!ssid.trim()) {
    throw new Error('SSID is required');
  }
  try {
    const result = await invoke<BleResult>('ble_write_wifi', { ssid, password });
    return result;
  } catch (error) {
    console.error('Write WiFi failed:', error);
    throw new Error(`Failed to write WiFi config: ${error}`);
  }
}

/**
 * Write LLM configuration to device
 */
export async function bleWriteLlm(model: string, apiKey: string): Promise<BleResult> {
  if (!model.trim()) {
    throw new Error('Model is required');
  }
  if (!apiKey.trim()) {
    throw new Error('API key is required');
  }
  try {
    const result = await invoke<BleResult>('ble_write_llm', { model, apiKey });
    return result;
  } catch (error) {
    console.error('Write LLM failed:', error);
    throw new Error(`Failed to write LLM config: ${error}`);
  }
}

/**
 * Write hostname to device
 */
export async function bleWriteHostname(hostname: string): Promise<BleResult> {
  if (!hostname.trim()) {
    throw new Error('Hostname is required');
  }
  try {
    const result = await invoke<BleResult>('ble_write_hostname', { hostname });
    return result;
  } catch (error) {
    console.error('Write hostname failed:', error);
    throw new Error(`Failed to write hostname: ${error}`);
  }
}

/**
 * Get connection status
 */
export async function bleStatus(): Promise<BleResult> {
  try {
    const result = await invoke<BleResult>('ble_status');
    return result;
  } catch (error) {
    console.error('Get status failed:', error);
    throw new Error(`Failed to get status: ${error}`);
  }
}

/**
 * Get system status from device
 */
export async function bleGetStatus(): Promise<SystemStatusResponse> {
  try {
    const result = await invoke<SystemStatusResponse>('ble_get_status');
    return result;
  } catch (error) {
    console.error('Get system status failed:', error);
    throw new Error(`Failed to get system status: ${error}`);
  }
}

/**
 * Get device information
 */
export async function bleGetDeviceInfo(): Promise<DeviceInfoResponse> {
  try {
    const result = await invoke<DeviceInfoResponse>('ble_get_device_info');
    return result;
  } catch (error) {
    console.error('Get device info failed:', error);
    throw new Error(`Failed to get device info: ${error}`);
  }
}

/**
 * Get WiFi status from device
 */
export async function bleGetWifiStatus(): Promise<WifiStatusResponse> {
  try {
    const result = await invoke<WifiStatusResponse>('ble_get_wifi_status');
    return result;
  } catch (error) {
    console.error('Get WiFi status failed:', error);
    throw new Error(`Failed to get WiFi status: ${error}`);
  }
}

/**
 * Get conversation log from device
 */
export async function bleGetConversations(): Promise<ConversationEntry[]> {
  try {
    const result = await invoke<ConversationsResponse>('ble_get_conversations');
    return result.conversations;
  } catch (error) {
    console.error('Get conversations failed:', error);
    throw new Error(`Failed to get conversations: ${error}`);
  }
}

export interface ChannelStatus {
  id: string;
  status: 'active' | 'inactive' | 'error';
  messages: number;
  lastActivity: number | null;
}

/**
 * Get communication channels status from device
 */
export async function bleGetChannels(): Promise<ChannelStatus[]> {
  try {
    const result = await invoke<ChannelStatus[]>('ble_get_channels');
    return result;
  } catch (error) {
    console.error('Get channels failed:', error);
    return [];
  }
}

/**
 * Reboot the connected device
 */
export async function bleReboot(): Promise<BleResult> {
  try {
    const result = await invoke<BleResult>('ble_reboot');
    return result;
  } catch (error) {
    console.error('Reboot failed:', error);
    throw new Error(`Reboot failed: ${error}`);
  }
}

export interface ExportConfigResponse {
  exported_at: string;
  device_id: string;
  config: {
    wifi_ssid: string | null;
    llm_model: string | null;
    hostname: string | null;
  };
}

/**
 * Export device configuration to JSON
 */
export async function bleExportConfig(): Promise<ExportConfigResponse> {
  try {
    const result = await invoke<ExportConfigResponse>('ble_export_config');
    return result;
  } catch (error) {
    console.error('Export config failed:', error);
    throw new Error(`Export config failed: ${error}`);
  }
}

export interface DeviceLogsResponse {
  success: boolean;
  count: number;
  logs: string[];
}

/**
 * Get device logs
 */
export async function bleGetLogs(lines: number = 100): Promise<DeviceLogsResponse> {
  try {
    const result = await invoke<DeviceLogsResponse>('ble_get_logs', { lines });
    return result;
  } catch (error) {
    console.error('Get logs failed:', error);
    throw new Error(`Get logs failed: ${error}`);
  }
}

export interface DiagnosticsResponse {
  success: boolean;
  timestamp: string;
  device?: {
    version: string;
    chip_model: string;
    uptime_ms: number;
  };
  wifi?: {
    state: number;
    rssi: number;
    ssid: string;
  };
  memory?: {
    total_bytes: number;
    free_bytes: number;
    usage_percent: number;
  };
  ble?: {
    connected: boolean;
    mtu: number;
  };
}

/**
 * Run diagnostics on connected device
 */
export async function bleDiagnostics(): Promise<DiagnosticsResponse> {
  try {
    const result = await invoke<DiagnosticsResponse>('ble_diagnostics');
    return result;
  } catch (error) {
    console.error('Diagnostics failed:', error);
    throw new Error(`Diagnostics failed: ${error}`);
  }
}

/**
 * Execute system command via BLE
 */
export async function bleExecCommand(command: string): Promise<BleResult> {
  if (!command.trim()) {
    throw new Error('Command is required');
  }
  try {
    const result = await invoke<BleResult>('ble_exec_command', { command });
    return result;
  } catch (error) {
    console.error('Exec command failed:', error);
    throw new Error(`Failed to execute command: ${error}`);
  }
}

// Re-export types for convenience
export type { BleDevice, DeviceConfig, BleResult };
