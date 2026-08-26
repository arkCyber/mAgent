import { useState, useCallback, useEffect, useRef } from 'react';
import { usbListPorts, usbOpen, usbGetStatus, usbClose } from './useUsb';

interface UseUsbAutoConnectReturn {
  /** True while listing ports on startup / refresh. */
  scanning: boolean;
  /** Candidate USB-serial port paths. */
  ports: string[];
  /** Path of the open port, or null when disconnected. */
  connectedPath: string | null;
  /** True while an auto-connect open is in flight. */
  connecting: boolean;
  /** Human-readable error (e.g. no port found / open failed). */
  error: string | null;
  /** Manually trigger a scan + connect again. */
  autoConnect: () => Promise<void>;
  /** Close the USB connection. */
  disconnect: () => Promise<void>;
}

/**
 * Auto-scan the USB-serial ports once on app startup and connect to the first
 * C61 bridge candidate found (no manual step needed).
 */
export function useUsbAutoConnect(): UseUsbAutoConnectReturn {
  const [scanning, setScanning] = useState(false);
  const [ports, setPorts] = useState<string[]>([]);
  const [connectedPath, setConnectedPath] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Refs mirror the connection state so we can decide idempotently and guard
  // against concurrency synchronously (before any `await`). This guarantees we
  // never re-open a port that is already connected — a common source of
  // "repeatedly connecting" after a successful link.
  const connectedRef = useRef<string | null>(null);
  const inFlightRef = useRef(false);

  const autoConnect = useCallback(async () => {
    // Already connected — never reopen the port.
    if (connectedRef.current) return;
    // A scan/connect is already running — don't stack another one.
    if (inFlightRef.current) return;

    inFlightRef.current = true;
    setError(null);
    try {
      // Reflect an existing backend connection (e.g. restored session / a remount
      // after a successful link) WITHOUT flashing the "scanning/connecting" UI.
      const status = await usbGetStatus();
      if (status.connected && status.path) {
        connectedRef.current = status.path;
        setConnectedPath(status.path);
        return;
      }

      setScanning(true);
      const found = await usbListPorts();
      setPorts(found);
      if (found.length === 0) {
        setError('No USB serial ports found');
        return;
      }

      // Prefer the first candidate found.
      const target = found[0];
      setConnecting(true);
      try {
        await usbOpen(target);
        connectedRef.current = target;
        setConnectedPath(target);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setConnecting(false);
      }
    } finally {
      inFlightRef.current = false;
      setScanning(false);
    }
  }, []);

  // Auto-scan + auto-connect once when the app mounts. The `started` ref guards
  // against React StrictMode double-invoking this effect in dev (which would
  // otherwise open the serial port twice).
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    autoConnect();
  }, [autoConnect]);

  const disconnect = useCallback(async () => {
    try {
      await usbClose();
    } finally {
      connectedRef.current = null;
      setConnectedPath(null);
    }
  }, []);

  return { scanning, ports, connectedPath, connecting, error, autoConnect, disconnect };
}
