import { describe, it, expect } from 'vitest';
import {
  parseCwLap,
  parseSafeMode,
  parseIdentityPublicKey,
  deriveAddress,
} from '../../utils/at';

describe('AT reply parsers', () => {
  describe('parseCwLap', () => {
    it('parses a single +CWLAP table row', () => {
      const rows = parseCwLap('+CWLAP:(3,"HomeWiFi",-45,6)\r\nOK');
      expect(rows).toEqual([{ auth: 3, ssid: 'HomeWiFi', rssi: -45, channel: 6 }]);
    });

    it('parses multiple rows in any order', () => {
      const rows = parseCwLap(
        '+CWLAP:(2,"Guest",-78,11)\r\n+CWLAP:(3,"Office",-62,149)\r\nOK'
      );
      expect(rows).toHaveLength(2);
      expect(rows[0]).toEqual({ auth: 2, ssid: 'Guest', rssi: -78, channel: 11 });
      expect(rows[1]).toEqual({ auth: 3, ssid: 'Office', rssi: -62, channel: 149 });
    });

    it('returns an empty array for the v0.2 "scan-started" reply', () => {
      expect(parseCwLap('+CWLAP:scan-started\r\nOK')).toEqual([]);
    });

    it('returns an empty array when there is no payload', () => {
      expect(parseCwLap('OK')).toEqual([]);
      expect(parseCwLap('')).toEqual([]);
    });
  });

  describe('parseSafeMode', () => {
    it('reads enabled state (1)', () => {
      expect(parseSafeMode('+SAFEMODE:1\r\nOK')).toBe(true);
    });

    it('reads disabled state (0)', () => {
      expect(parseSafeMode('+SAFEMODE:0\r\nOK')).toBe(false);
    });

    it('tolerates surrounding whitespace', () => {
      expect(parseSafeMode('+SAFEMODE: 1 OK')).toBe(true);
    });

    it('returns null when the flag cannot be decoded', () => {
      expect(parseSafeMode('Command executed')).toBeNull();
    });
  });

  describe('parseIdentityPublicKey', () => {
    it('parses +IDENT public key', () => {
      const key = 'a'.repeat(64);
      expect(parseIdentityPublicKey(`+IDENT:${key}\r\nOK`)).toBe(key);
    });

    it('parses +IDENTROT public key', () => {
      const key = 'b'.repeat(64);
      expect(parseIdentityPublicKey(`+IDENTROT:${key}\r\nOK`)).toBe(key);
    });

    it('returns null for NO_IDENTITY', () => {
      expect(parseIdentityPublicKey('+IDENT:NO_IDENTITY\r\nOK')).toBeNull();
    });

    it('returns null when the reply cannot be decoded', () => {
      expect(parseIdentityPublicKey('Command executed')).toBeNull();
    });
  });

  describe('deriveAddress', () => {
    it('derives the last 40 hex chars as a 0x-prefixed address', () => {
      const key = 'a'.repeat(24) + '1234567890abcdef1234567890abcdef12345678';
      expect(deriveAddress(key)).toBe('0x1234567890abcdef1234567890abcdef12345678');
    });

    it('strips an existing 0x prefix', () => {
      const key = '0x' + '9'.repeat(40);
      expect(deriveAddress(key)).toBe('0x' + '9'.repeat(40));
    });

    it('falls back to a zero address for a short key', () => {
      expect(deriveAddress('0x00')).toBe('0x0000000000000000000000000000000000000000');
    });
  });
});
