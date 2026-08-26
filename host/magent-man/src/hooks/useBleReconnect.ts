import { useRef, useCallback, useEffect, useState } from 'react';
import { bleConnect, bleDisconnect, bleStatus, type BleDevice } from './useBle';

interface UseBleReconnectOptions {
  maxRetries?: number;
  baseDelay?: number;
  maxDelay?: number;
  onConnected?: () => void;
  onDisconnected?: () => void;
  onError?: (error: Error) => void;
}

interface UseBleReconnectReturn {
  connect: (device: BleDevice) => Promise<boolean>;
  disconnect: () => Promise<void>;
  isReconnecting: boolean;
  retryCount: number;
  cancelReconnect: () => void;
}

export function useBleReconnect(options: UseBleReconnectOptions = {}): UseBleReconnectReturn {
  const {
    maxRetries = 5,
    baseDelay = 1000,
    maxDelay = 30000,
    onConnected,
    onDisconnected,
    onError,
  } = options;

  const isReconnecting = useRef(false);
  const retryCount = useRef(0);
  const currentDevice = useRef<BleDevice | null>(null);
  const reconnectTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isMounted = useRef(true);

  // Reactive mirrors so components re-render when the reconnect state changes.
  const [reconnecting, setReconnecting] = useState(false);
  const [attempt, setAttempt] = useState(0);

  const markReconnecting = useCallback((value: boolean) => {
    isReconnecting.current = value;
    setReconnecting(value);
  }, []);

  const bumpRetry = useCallback(() => {
    retryCount.current += 1;
    setAttempt(retryCount.current);
  }, []);

  // Calculate exponential backoff delay
  const getBackoffDelay = useCallback(
    (attempt: number): number => {
      const delay = Math.min(baseDelay * Math.pow(2, attempt), maxDelay);
      // Add jitter (±25%)
      const jitter = delay * 0.25 * (Math.random() * 2 - 1);
      return Math.round(delay + jitter);
    },
    [baseDelay, maxDelay]
  );

  // Attempt to connect with retries
  const connectWithRetry = useCallback(
    async (device: BleDevice): Promise<boolean> => {
      if (!isMounted.current) return false;

      try {
        const result = await bleConnect(device.id);

        if (result.success) {
          currentDevice.current = device;
          retryCount.current = 0;
          setAttempt(0);
          markReconnecting(false);
          onConnected?.();
          return true;
        }

        // Connection failed, schedule retry if we have attempts left
        if (retryCount.current < maxRetries) {
          markReconnecting(true);
          const delay = getBackoffDelay(retryCount.current);
          bumpRetry();

          reconnectTimeout.current = setTimeout(() => {
            if (isMounted.current && currentDevice.current) {
              connectWithRetry(currentDevice.current);
            }
          }, delay);
          return false;
        }

        // Max retries exceeded
        markReconnecting(false);
        onError?.(new Error('Max connection retries exceeded'));
        return false;
      } catch (error) {
        if (!isMounted.current) return false;

        if (retryCount.current < maxRetries) {
          markReconnecting(true);
          const delay = getBackoffDelay(retryCount.current);
          bumpRetry();

          reconnectTimeout.current = setTimeout(() => {
            if (isMounted.current && currentDevice.current) {
              connectWithRetry(currentDevice.current);
            }
          }, delay);
          return false;
        }

        markReconnecting(false);
        onError?.(error instanceof Error ? error : new Error('Connection failed'));
        return false;
      }
    },
    [maxRetries, getBackoffDelay, onConnected, onError, markReconnecting, bumpRetry]
  );

  // Connect function
  const connect = useCallback(
    async (device: BleDevice): Promise<boolean> => {
      // Cancel any pending reconnection
      if (reconnectTimeout.current) {
        clearTimeout(reconnectTimeout.current);
        reconnectTimeout.current = null;
      }

      retryCount.current = 0;
      setAttempt(0);
      markReconnecting(false);
      currentDevice.current = device;

      return connectWithRetry(device);
    },
    [connectWithRetry, markReconnecting]
  );

  // Disconnect function
  const disconnect = useCallback(async (): Promise<void> => {
    // Cancel any pending reconnection
    if (reconnectTimeout.current) {
      clearTimeout(reconnectTimeout.current);
      reconnectTimeout.current = null;
    }

    markReconnecting(false);
    retryCount.current = 0;
    setAttempt(0);
    currentDevice.current = null;

    try {
      await bleDisconnect();
      onDisconnected?.();
    } catch (error) {
      // Ignore disconnect errors
      console.error('Disconnect error:', error);
    }
  }, [onDisconnected, markReconnecting]);

  // Cancel reconnection attempts
  const cancelReconnect = useCallback(() => {
    if (reconnectTimeout.current) {
      clearTimeout(reconnectTimeout.current);
      reconnectTimeout.current = null;
    }
    markReconnecting(false);
    retryCount.current = 0;
    setAttempt(0);
    currentDevice.current = null;
  }, [markReconnecting]);

  // Cleanup on unmount
  useEffect(() => {
    isMounted.current = true;
    return () => {
      isMounted.current = false;
      if (reconnectTimeout.current) {
        clearTimeout(reconnectTimeout.current);
      }
    };
  }, []);

  return {
    connect,
    disconnect,
    isReconnecting: reconnecting,
    retryCount: attempt,
    cancelReconnect,
  };
}

// Hook for automatic reconnection on disconnect
export function useBleAutoReconnect(
  deviceId: string | null,
  options: UseBleReconnectOptions & { enabled?: boolean } = {}
) {
  const { enabled = true, ...reconnectOptions } = options;
  const reconnect = useBleReconnect(reconnectOptions);
  const lastDeviceRef = useRef<BleDevice | null>(null);

  // Store last connected device
  useEffect(() => {
    if (deviceId) {
      lastDeviceRef.current = {
        id: deviceId,
        name: `Device ${deviceId.slice(0, 8)}`,
        rssi: 0,
      };
    }
  }, [deviceId]);

  // Monitor for disconnections and auto-reconnect
  // HARDENING (audit-2026-08): the interval callback calls `reconnect.connect()`
  // which schedules a `setTimeout` that persists beyond the interval's lifetime.
  // We guard with `isMounted` to prevent dangling timeouts after unmount.
  const reconnectRef = useRef(reconnect);
  reconnectRef.current = reconnect;

  useEffect(() => {
    if (!enabled || !deviceId) return;

    const interval = setInterval(async () => {
      try {
        const status = await bleStatus();

        if (!status.success && lastDeviceRef.current) {
          console.log('Device disconnected, attempting reconnection...');
          // Only attempt reconnect if still mounted; the interval itself is
          // cleared by the cleanup return below, but a concurrent iteration
          // could have already fired before unmount.
          if (reconnectRef.current) {
            await reconnectRef.current.connect(lastDeviceRef.current);
          }
        }
      } catch (e) {
        // Network errors are logged by bleStatus but do not warrant crashing
        // the polling loop. We still log at debug level for diagnostics.
        console.debug('[BLE reconnect] status poll error:', e);
      }
    }, 5000);

    return () => clearInterval(interval);
  }, [enabled, deviceId]);

  return reconnect;
}
