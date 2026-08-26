/**
 * Secure storage utilities for sensitive data
 * Uses Web Crypto API for encryption/decryption
 */

const STORAGE_PREFIX = 'magent_';
const ENCRYPTED_PREFIX = 'enc_';

// Check if Web Crypto API is available
const isCryptoAvailable = (): boolean => {
  return typeof window !== 'undefined' && !!window.crypto?.subtle;
};

/**
 * Derive a key from password using PBKDF2
 */
async function deriveKey(password: string, salt: Uint8Array): Promise<CryptoKey> {
  const encoder = new TextEncoder();
  const passwordKey = await window.crypto.subtle.importKey(
    'raw',
    encoder.encode(password),
    'PBKDF2',
    false,
    ['deriveKey']
  );

  return window.crypto.subtle.deriveKey(
    {
      name: 'PBKDF2',
      salt,
      iterations: 100000,
      hash: 'SHA-256',
    },
    passwordKey,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt']
  );
}

/**
 * Encrypt data using AES-GCM
 */
export async function encrypt(data: string, deviceId: string): Promise<string> {
  if (!isCryptoAvailable()) {
    console.warn('Web Crypto API not available, using base64 encoding');
    return btoa(data);
  }

  try {
    const encoder = new TextEncoder();
    const salt = window.crypto.getRandomValues(new Uint8Array(16));
    const iv = window.crypto.getRandomValues(new Uint8Array(12));

    // Use device ID as part of the password for device-specific encryption
    const key = await deriveKey(deviceId, salt);

    const encrypted = await window.crypto.subtle.encrypt(
      { name: 'AES-GCM', iv },
      key,
      encoder.encode(data)
    );

    // Combine salt + iv + encrypted data
    const combined = new Uint8Array(salt.length + iv.length + encrypted.byteLength);
    combined.set(salt, 0);
    combined.set(iv, salt.length);
    combined.set(new Uint8Array(encrypted), salt.length + iv.length);

    // Return as base64 with prefix
    return ENCRYPTED_PREFIX + btoa(String.fromCharCode(...combined));
  } catch (error) {
    console.error('Encryption failed:', error);
    throw new Error('Failed to encrypt data');
  }
}

/**
 * Decrypt data using AES-GCM
 */
export async function decrypt(encryptedData: string, deviceId: string): Promise<string> {
  // Check if this was encrypted or just base64 encoded
  if (!encryptedData.startsWith(ENCRYPTED_PREFIX)) {
    if (isCryptoAvailable()) {
      try {
        return atob(encryptedData);
      } catch {
        throw new Error('Failed to decode data');
      }
    }
    return encryptedData;
  }

  if (!isCryptoAvailable()) {
    throw new Error('Web Crypto API not available for decryption');
  }

  try {
    const combined = Uint8Array.from(atob(encryptedData.slice(ENCRYPTED_PREFIX.length)), (c) =>
      c.charCodeAt(0)
    );

    const salt = combined.slice(0, 16);
    const iv = combined.slice(16, 28);
    const data = combined.slice(28);

    const key = await deriveKey(deviceId, salt);

    const decrypted = await window.crypto.subtle.decrypt(
      { name: 'AES-GCM', iv },
      key,
      data
    );

    return new TextDecoder().decode(decrypted);
  } catch (error) {
    console.error('Decryption failed:', error);
    throw new Error('Failed to decrypt data');
  }
}

/**
 * Secure storage class
 */
export class SecureStorage {
  /**
   * Save encrypted data to localStorage
   */
  static async set(key: string, value: string, deviceId: string): Promise<void> {
    const encrypted = await encrypt(value, deviceId);
    localStorage.setItem(STORAGE_PREFIX + key, encrypted);
  }

  /**
   * Load and decrypt data from localStorage
   */
  static async get(key: string, deviceId: string): Promise<string | null> {
    const encrypted = localStorage.getItem(STORAGE_PREFIX + key);
    if (!encrypted) return null;

    try {
      return await decrypt(encrypted, deviceId);
    } catch {
      // Return null if decryption fails
      return null;
    }
  }

  /**
   * Remove data from localStorage
   */
  static remove(key: string): void {
    localStorage.removeItem(STORAGE_PREFIX + key);
  }

  /**
   * Check if key exists
   */
  static has(key: string): boolean {
    return localStorage.getItem(STORAGE_PREFIX + key) !== null;
  }
}

/**
 * Save API key securely
 */
export async function saveApiKey(deviceId: string, apiKey: string): Promise<void> {
  await SecureStorage.set('api_key', apiKey, deviceId);
}

/**
 * Load API key securely
 */
export async function loadApiKey(deviceId: string): Promise<string | null> {
  return SecureStorage.get('api_key', deviceId);
}

/**
 * Clear API key
 */
export function clearApiKey(): void {
  SecureStorage.remove('api_key');
}

/**
 * Save WiFi password securely
 */
export async function saveWifiPassword(deviceId: string, password: string): Promise<void> {
  await SecureStorage.set('wifi_password', password, deviceId);
}

/**
 * Load WiFi password securely
 */
export async function loadWifiPassword(deviceId: string): Promise<string | null> {
  return SecureStorage.get('wifi_password', deviceId);
}

/**
 * Clear WiFi password
 */
export function clearWifiPassword(): void {
  SecureStorage.remove('wifi_password');
}
