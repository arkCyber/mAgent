/**
 * Parsers for the mAgent AT protocol replies used by the desktop UI.
 *
 * Kept pure (no React / Tauri imports) so the parsing logic can be unit-tested
 * in isolation.
 */

export interface CwLapEntry {
  auth: number;
  ssid: string;
  rssi: number;
  channel: number;
}

/**
 * Parse `+CWLAP:(auth,"ssid",rssi,channel)` rows returned by the Wi-Fi scan.
 * Returns an empty array when no table rows are present (v0.2 firmware replies
 * with `+CWLAP:scan-started` and emits the table asynchronously).
 */
export function parseCwLap(raw: string): CwLapEntry[] {
  const entries: CwLapEntry[] = [];
  const rowRe = /\+CWLAP:\s*\((\d+),"(.*?)",(-?\d+),(\d+)\)/g;
  let m: RegExpExecArray | null;
  while ((m = rowRe.exec(raw)) !== null) {
    entries.push({
      auth: Number(m[1]),
      ssid: m[2],
      rssi: Number(m[3]),
      channel: Number(m[4]),
    });
  }
  return entries;
}

/**
 * Parse the safe-mode flag from `AT+SAFEMODE?` (`+SAFEMODE:<0/1>`).
 * Returns `null` when the reply cannot be decoded.
 */
export function parseSafeMode(raw: string): boolean | null {
  const match = /\+SAFEMODE:\s*([01])/.exec(raw);
  if (!match) return null;
  return match[1] === '1';
}

/**
 * Parse the Ed25519 public key from `AT+IDENT?` (`+IDENT:<hex>`) or
 * `AT+IDENTROT` (`+IDENTROT:<hex>`). Returns `null` when the device reports
 * `NO_IDENTITY` or the reply cannot be decoded.
 */
export function parseIdentityPublicKey(raw: string): string | null {
  if (/NO_IDENTITY/i.test(raw)) return null;
  const match = /\+IDENT(?:ROT)?:\s*"?([0-9a-fA-F]+)"?/.exec(raw);
  if (!match) return null;
  return match[1];
}

/**
 * Derive a compact EVM-style address (last 40 hex chars, `0x`-prefixed) from a
 * hex public key. This is only a display helper — it is not a real keccak
 * checksum, which the embedded firmware does not expose over AT.
 */
export function deriveAddress(publicKeyHex: string): string {
  const clean = publicKeyHex.startsWith('0x') ? publicKeyHex.slice(2) : publicKeyHex;
  // Left-pad with zeros so the address is always a consistent 40 hex chars.
  return `0x${clean.slice(-40).padStart(40, '0')}`;
}
