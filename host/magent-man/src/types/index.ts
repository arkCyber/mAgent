// mAgent-Man Type Definitions

export interface BleDevice {
  id: string;
  name: string;
  rssi: number;
}

export interface DeviceConfig {
  wifi_ssid: string | null;
  wifi_password: string | null;
  llm_model: string | null;
  llm_api_key: string | null;
  hostname: string | null;
}

export interface BleResult {
  success: boolean;
  message: string;
  data?: unknown;
}

export type ConnectionState = 'disconnected' | 'connecting' | 'connected' | 'error';

export interface AppState {
  connectionState: ConnectionState;
  connectedDevice: BleDevice | null;
  devices: BleDevice[];
  config: DeviceConfig | null;
  error: string | null;
}
