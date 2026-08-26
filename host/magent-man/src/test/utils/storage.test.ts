import { describe, it, expect, beforeEach } from 'vitest';
import {
  ConfigStorage,
  ChatStorage,
  DeviceStorage,
  STORAGE_KEYS,
  type StoredConfig,
} from '../../utils/storage';

describe('ConfigStorage', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('returns an empty map by default', () => {
    expect(ConfigStorage.getConfigs()).toEqual({});
    expect(ConfigStorage.getConfig('dev-1')).toBeNull();
    expect(ConfigStorage.getMostRecentConfig()).toBeNull();
  });

  it('saves and reads a full config for a device', () => {
    ConfigStorage.saveConfig('dev-1', {
      wifi_ssid: 'HomeWiFi',
      llm_model: 'deepseek-chat',
      hostname: 'magent-001',
    });

    const config = ConfigStorage.getConfig('dev-1');
    expect(config).not.toBeNull();
    expect(config?.wifi_ssid).toBe('HomeWiFi');
    expect(config?.llm_model).toBe('deepseek-chat');
    expect(config?.hostname).toBe('magent-001');
    expect(config?.lastUpdated).toBeGreaterThan(0);
  });

  it('merges partial updates and preserves previous fields', () => {
    ConfigStorage.saveConfig('dev-1', { wifi_ssid: 'HomeWiFi' });
    ConfigStorage.saveConfig('dev-1', { llm_model: 'deepseek-chat' });

    const config = ConfigStorage.getConfig('dev-1');
    expect(config?.wifi_ssid).toBe('HomeWiFi');
    expect(config?.llm_model).toBe('deepseek-chat');
  });

  it('deletes a device config', () => {
    ConfigStorage.saveConfig('dev-1', { wifi_ssid: 'HomeWiFi' });
    ConfigStorage.deleteConfig('dev-1');
    expect(ConfigStorage.getConfig('dev-1')).toBeNull();
  });

  it('returns the most recently updated config', () => {
    const older: StoredConfig = {
      wifi_ssid: 'A', wifi_password: null, llm_model: null,
      llm_api_key: null, hostname: null, lastUpdated: 1000,
    };
    const newer: StoredConfig = {
      wifi_ssid: 'B', wifi_password: null, llm_model: null,
      llm_api_key: null, hostname: null, lastUpdated: 2000,
    };
    localStorage.setItem(STORAGE_KEYS.DEVICE_CONFIG, JSON.stringify({ a: older, b: newer }));

    expect(ConfigStorage.getMostRecentConfig()).toEqual({ deviceId: 'b', config: newer });
  });

  it('returns the default stub for an unknown device on first save', () => {
    ConfigStorage.saveConfig('new-dev', { wifi_ssid: 'X' });
    const config = ConfigStorage.getConfig('new-dev');
    expect(config?.wifi_password).toBeNull();
    expect(config?.llm_api_key).toBeNull();
  });
});

describe('ChatStorage', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('creates and reads back a session', () => {
    const session = ChatStorage.createSession('dev-1', 'mAgent-001');
    expect(session.deviceId).toBe('dev-1');
    expect(session.messages).toEqual([]);
    expect(ChatStorage.getSessionsForDevice('dev-1')).toHaveLength(1);
    expect(ChatStorage.getLatestSession('dev-1')?.id).toBe(session.id);
  });

  it('returns null for a device with no sessions', () => {
    expect(ChatStorage.getLatestSession('missing')).toBeNull();
  });
});

describe('DeviceStorage', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('adds a recent device and keeps it at most 10 entries', () => {
    for (let i = 0; i < 12; i++) {
      DeviceStorage.addRecentDevice({ id: `d-${i}`, name: `dev ${i}` });
    }
    const recent = DeviceStorage.getRecentDevices();
    expect(recent.length).toBeLessThanOrEqual(10);
    // The most recently added device is at the front.
    expect(recent[0].id).toBe('d-11');
  });

  it('removes a recent device and clears all', () => {
    DeviceStorage.addRecentDevice({ id: 'a', name: 'A' });
    DeviceStorage.addRecentDevice({ id: 'b', name: 'B' });
    DeviceStorage.removeRecentDevice('a');
    expect(DeviceStorage.getRecentDevices().map((d) => d.id)).toEqual(['b']);
    DeviceStorage.clearAll();
    expect(DeviceStorage.getRecentDevices()).toEqual([]);
  });
});
