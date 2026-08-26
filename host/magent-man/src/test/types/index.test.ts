import { describe, it, expect } from 'vitest';
import type { BleDevice, DeviceConfig, BleResult, ConnectionState } from '../../types';

describe('Types', () => {
  describe('BleDevice', () => {
    it('should have required properties', () => {
      const device: BleDevice = {
        id: 'device-123',
        name: 'mAgent-001',
        rssi: -45,
      };

      expect(device.id).toBe('device-123');
      expect(device.name).toBe('mAgent-001');
      expect(device.rssi).toBe(-45);
    });

    it('should accept valid RSSI range', () => {
      const weakDevice: BleDevice = { id: '1', name: 'Device', rssi: -90 };
      const strongDevice: BleDevice = { id: '2', name: 'Device', rssi: -30 };

      expect(weakDevice.rssi).toBeLessThan(strongDevice.rssi);
    });
  });

  describe('DeviceConfig', () => {
    it('should allow null values', () => {
      const config: DeviceConfig = {
        wifi_ssid: null,
        wifi_password: null,
        llm_model: null,
        llm_api_key: null,
        hostname: null,
      };

      expect(config.wifi_ssid).toBeNull();
      expect(config.llm_model).toBeNull();
    });

    it('should accept full configuration', () => {
      const config: DeviceConfig = {
        wifi_ssid: 'MyNetwork',
        wifi_password: 'password123',
        llm_model: 'deepseek-chat',
        llm_api_key: 'sk-api-key',
        hostname: 'magent-001',
      };

      expect(config.wifi_ssid).toBe('MyNetwork');
      expect(config.llm_api_key).toBe('sk-api-key');
    });

    it('should allow partial configuration', () => {
      const config: DeviceConfig = {
        wifi_ssid: 'MyNetwork',
        wifi_password: null,
        llm_model: 'deepseek-chat',
        llm_api_key: null,
        hostname: null,
      };

      expect(config.wifi_ssid).toBe('MyNetwork');
      expect(config.llm_model).toBe('deepseek-chat');
      expect(config.hostname).toBeNull();
    });
  });

  describe('BleResult', () => {
    it('should represent success', () => {
      const result: BleResult = {
        success: true,
        message: 'Operation completed',
      };

      expect(result.success).toBe(true);
      expect(result.message).toBe('Operation completed');
    });

    it('should represent failure', () => {
      const result: BleResult = {
        success: false,
        message: 'Operation failed',
      };

      expect(result.success).toBe(false);
      expect(result.message).toBe('Operation failed');
    });

    it('should allow optional data', () => {
      const result: BleResult = {
        success: true,
        message: 'Success',
        data: { key: 'value' },
      };

      expect(result.data).toBeDefined();
      expect((result.data as any).key).toBe('value');
    });
  });

  describe('ConnectionState', () => {
    it('should accept disconnected state', () => {
      const state: ConnectionState = 'disconnected';
      expect(state).toBe('disconnected');
    });

    it('should accept connecting state', () => {
      const state: ConnectionState = 'connecting';
      expect(state).toBe('connecting');
    });

    it('should accept connected state', () => {
      const state: ConnectionState = 'connected';
      expect(state).toBe('connected');
    });

    it('should accept error state', () => {
      const state: ConnectionState = 'error';
      expect(state).toBe('error');
    });
  });
});
