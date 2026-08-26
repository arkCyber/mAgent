/**
 * Local Storage Utilities for mAgent-Man
 * Provides persistent storage for chat history and configurations
 */

// Storage keys
const STORAGE_KEYS = {
  CHAT_HISTORY: 'magent_chat_history',
  DEVICE_CONFIG: 'magent_device_config',
  RECENT_DEVICES: 'magent_recent_devices',
  APP_SETTINGS: 'magent_app_settings',
} as const;

// Generic storage helper
function getItem<T>(key: string, defaultValue: T): T {
  try {
    const item = localStorage.getItem(key);
    if (item === null) return defaultValue;
    return JSON.parse(item) as T;
  } catch {
    return defaultValue;
  }
}

function setItem<T>(key: string, value: T): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch (error) {
    console.error(`Failed to save ${key}:`, error);
  }
}

// Types
export interface ChatMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  text: string;
  timestamp: number;
  deviceId?: string;
}

export interface ChatSession {
  id: string;
  deviceId: string;
  deviceName: string;
  messages: ChatMessage[];
  createdAt: number;
  updatedAt: number;
}

export interface StoredConfig {
  wifi_ssid: string | null;
  wifi_password: string | null;
  llm_model: string | null;
  llm_api_key: string | null;
  hostname: string | null;
  lastUpdated: number;
}

export interface RecentDevice {
  id: string;
  name: string;
  lastConnected: number;
}

// Chat History Storage
export const ChatStorage = {
  /**
   * Get all chat sessions
   */
  getSessions(): ChatSession[] {
    return getItem<ChatSession[]>(STORAGE_KEYS.CHAT_HISTORY, []);
  },

  /**
   * Get sessions for a specific device
   */
  getSessionsForDevice(deviceId: string): ChatSession[] {
    const sessions = this.getSessions();
    return sessions.filter(s => s.deviceId === deviceId);
  },

  /**
   * Get the most recent session for a device
   */
  getLatestSession(deviceId: string): ChatSession | null {
    const sessions = this.getSessionsForDevice(deviceId);
    if (sessions.length === 0) return null;
    const sorted = sessions.sort((a, b) => b.updatedAt - a.updatedAt);
    // HARDENING (audit-2026-08): after sorting, `sorted` could be
    // empty if the array was mutated between the length check and the
    // sort (e.g. concurrent storage writes in a background task).
    // Return null instead of crashing on `sorted[0]`.
    if (sorted.length === 0) return null;
    return sorted[0];
  },

  /**
   * Save a chat session
   */
  saveSession(session: ChatSession): void {
    const sessions = this.getSessions();
    const existingIndex = sessions.findIndex(s => s.id === session.id);

    if (existingIndex >= 0) {
      sessions[existingIndex] = { ...session, updatedAt: Date.now() };
    } else {
      sessions.push(session);
    }

    setItem(STORAGE_KEYS.CHAT_HISTORY, sessions);
  },

  /**
   * Create a new session for a device
   */
  createSession(deviceId: string, deviceName: string): ChatSession {
    const session: ChatSession = {
      id: `session_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
      deviceId,
      deviceName,
      messages: [],
      createdAt: Date.now(),
      updatedAt: Date.now(),
    };
    this.saveSession(session);
    return session;
  },

  /**
   * Add a message to a session
   */
  addMessage(sessionId: string, message: Omit<ChatMessage, 'id' | 'timestamp'>): void {
    const sessions = this.getSessions();
    const session = sessions.find(s => s.id === sessionId);

    if (session) {
      session.messages.push({
        ...message,
        id: `msg_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
        timestamp: Date.now(),
      });
      session.updatedAt = Date.now();
      setItem(STORAGE_KEYS.CHAT_HISTORY, sessions);
    }
  },

  /**
   * Clear all messages in a session
   */
  clearSession(sessionId: string): void {
    const sessions = this.getSessions();
    const session = sessions.find(s => s.id === sessionId);

    if (session) {
      session.messages = [];
      session.updatedAt = Date.now();
      setItem(STORAGE_KEYS.CHAT_HISTORY, sessions);
    }
  },

  /**
   * Delete a session
   */
  deleteSession(sessionId: string): void {
    const sessions = this.getSessions();
    const filtered = sessions.filter(s => s.id !== sessionId);
    setItem(STORAGE_KEYS.CHAT_HISTORY, filtered);
  },

  /**
   * Clear all chat history
   */
  clearAll(): void {
    localStorage.removeItem(STORAGE_KEYS.CHAT_HISTORY);
  },

  /**
   * Get storage size info
   */
  getStorageInfo(): { sessionCount: number; totalMessages: number; sizeBytes: number } {
    const sessions = this.getSessions();
    const totalMessages = sessions.reduce((sum, s) => sum + s.messages.length, 0);
    const rawSize = localStorage.getItem(STORAGE_KEYS.CHAT_HISTORY)?.length || 0;

    return {
      sessionCount: sessions.length,
      totalMessages,
      sizeBytes: rawSize,
    };
  },
};

// Device Config Storage
export const ConfigStorage = {
  /**
   * Get stored device configurations
   */
  getConfigs(): Record<string, StoredConfig> {
    return getItem<Record<string, StoredConfig>>(STORAGE_KEYS.DEVICE_CONFIG, {});
  },

  /**
   * Get config for a specific device
   */
  getConfig(deviceId: string): StoredConfig | null {
    const configs = this.getConfigs();
    return configs[deviceId] || null;
  },

  /**
   * Save config for a device
   */
  saveConfig(deviceId: string, config: Partial<StoredConfig>): void {
    const configs = this.getConfigs();
    const existing = configs[deviceId] || {
      wifi_ssid: null,
      wifi_password: null,
      llm_model: null,
      llm_api_key: null,
      hostname: null,
      lastUpdated: 0,
    };

    configs[deviceId] = {
      ...existing,
      ...config,
      lastUpdated: Date.now(),
    };

    setItem(STORAGE_KEYS.DEVICE_CONFIG, configs);
  },

  /**
   * Delete config for a device
   */
  deleteConfig(deviceId: string): void {
    const configs = this.getConfigs();
    delete configs[deviceId];
    setItem(STORAGE_KEYS.DEVICE_CONFIG, configs);
  },

  /**
   * Get the most recently configured device
   */
  getMostRecentConfig(): { deviceId: string; config: StoredConfig } | null {
    const configs = this.getConfigs();
    const entries = Object.entries(configs);

    if (entries.length === 0) return null;

    const sorted = entries.sort(([, a], [, b]) => b.lastUpdated - a.lastUpdated);
    // HARDENING (audit-2026-08 frontend): after sorting, `sorted` could
    // be empty if `entries` was mutated between the length check and the
    // sort (e.g. concurrent storage writes in background tasks).
    if (sorted.length === 0) return null;
    const [deviceId, config] = sorted[0];

    return { deviceId, config };
  },
};

// Recent Devices Storage
export const DeviceStorage = {
  /**
   * Get recently connected devices
   */
  getRecentDevices(): RecentDevice[] {
    return getItem<RecentDevice[]>(STORAGE_KEYS.RECENT_DEVICES, []);
  },

  /**
   * Add or update a recently connected device
   */
  addRecentDevice(device: Omit<RecentDevice, 'lastConnected'>): void {
    const devices = this.getRecentDevices();
    const existingIndex = devices.findIndex(d => d.id === device.id);

    const updatedDevice: RecentDevice = {
      ...device,
      lastConnected: Date.now(),
    };

    if (existingIndex >= 0) {
      devices[existingIndex] = updatedDevice;
    } else {
      devices.unshift(updatedDevice);
    }

    // Keep only the most recent 10 devices
    const trimmed = devices.slice(0, 10);
    setItem(STORAGE_KEYS.RECENT_DEVICES, trimmed);
  },

  /**
   * Remove a device from recent list
   */
  removeRecentDevice(deviceId: string): void {
    const devices = this.getRecentDevices();
    const filtered = devices.filter(d => d.id !== deviceId);
    setItem(STORAGE_KEYS.RECENT_DEVICES, filtered);
  },

  /**
   * Clear all recent devices
   */
  clearAll(): void {
    localStorage.removeItem(STORAGE_KEYS.RECENT_DEVICES);
  },
};

// Export storage keys for external use
export { STORAGE_KEYS };
