import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useAppInfo } from '../hooks/useAppInfo';
import type { ConnectionState } from '../types';

interface FooterProps {
  connectionState: ConnectionState;
  /** Name of the currently connected device (when any). */
  connectedDeviceName?: string | null;
  deviceCount: number;
  lastScanTime: Date | null;
  /** When provided, renders a Scan button that triggers a BLE scan. */
  onScan?: () => void;
  scanning?: boolean;
}

/**
 * Bottom status bar for the window: live connection status, discovered-device
 * count / last scan, an optional Scan action, plus app version and a live clock.
 */
export function Footer({
  connectionState,
  connectedDeviceName,
  deviceCount,
  lastScanTime,
  onScan,
  scanning = false,
}: FooterProps) {
  const { t } = useTranslation();
  const appInfo = useAppInfo();
  const [currentTime, setCurrentTime] = useState(new Date());

  useEffect(() => {
    const interval = setInterval(() => setCurrentTime(new Date()), 1000);
    return () => clearInterval(interval);
  }, []);

  const getBluetoothStatus = () => {
    switch (connectionState) {
      case 'connected':
        return {
          dot: '#22c55e',
          color: 'var(--color-success)',
          label: connectedDeviceName
            ? `● ${connectedDeviceName}`
            : `● ${t('footer.connected')}`,
          pulse: true,
        };
      case 'connecting':
        return {
          dot: '#eab308',
          color: 'var(--color-warning)',
          label: `◐ ${t('status.connecting')}`,
          pulse: true,
        };
      case 'error':
        return {
          dot: '#ef4444',
          color: 'var(--color-error)',
          label: `✕ ${t('status.error')}`,
          pulse: false,
        };
      default:
        return {
          dot: '#94a3b8',
          color: 'var(--color-text-muted)',
          label: `○ ${t('status.disconnected')}`,
          pulse: false,
        };
    }
  };

  const bluetoothStatus = getBluetoothStatus();

  const formatLastScan = () => {
    if (!lastScanTime) return t('footer.neverScanned');
    const diff = Math.floor((currentTime.getTime() - lastScanTime.getTime()) / 1000);
    if (diff < 60) return `${diff}s ${t('footer.ago')}`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ${t('footer.ago')}`;
    return lastScanTime.toLocaleTimeString();
  };

  return (
    <footer
      className="flex items-center justify-between gap-4 px-6 py-2 border-t text-xs flex-shrink-0 transition-colors duration-300"
      style={{
        backgroundColor: 'var(--color-surface)',
        borderColor: 'var(--color-border)',
        color: 'var(--color-text-secondary)',
      }}
    >
      <div className="flex items-center gap-4 min-w-0">
        {/* Connection Status */}
        <span
          className={`flex items-center gap-1.5 font-medium ${bluetoothStatus.pulse ? 'animate-pulse' : ''}`}
          style={{ color: bluetoothStatus.color }}
        >
          <span className="w-2 h-2 rounded-full" style={{ backgroundColor: bluetoothStatus.dot }} />
          <span className="truncate">{bluetoothStatus.label}</span>
        </span>

        <span className="opacity-30">|</span>

        {/* Devices Found */}
        <span className="flex items-center gap-1.5">
          <span>📡</span>
          <span>{deviceCount} {t('footer.devicesFound')}</span>
        </span>

        <span className="opacity-30">|</span>

        {/* Last Scan */}
        <span className="flex items-center gap-1.5">
          <span>🕐</span>
          <span>{t('footer.lastScan')}: {formatLastScan()}</span>
        </span>
      </div>

      <div className="flex items-center gap-4">
        {/* Scan action */}
        {onScan && (
          <button
            onClick={onScan}
            disabled={scanning}
            className="flex items-center gap-1.5 px-3 py-1 rounded-md text-xs font-medium transition-all duration-200 hover:scale-105 disabled:opacity-50"
            style={{ backgroundColor: 'var(--color-primary-light)', color: 'var(--color-primary)' }}
          >
            {scanning ? (
              <>
                <span
                  className="w-3 h-3 border-2 rounded-full animate-spin"
                  style={{ borderColor: 'var(--color-primary-light)', borderTopColor: 'var(--color-primary)' }}
                />
                {t('devices.scanning')}
              </>
            ) : (
              <>
                <span>🔍</span>
                {t('devices.scan')}
              </>
            )}
          </button>
        )}

        {/* App Version */}
        <span className="flex items-center gap-1.5">
          <span>ℹ️</span>
          <span className="font-mono">{appInfo.name} v{appInfo.version}</span>
        </span>

        {/* Current Time */}
        <span className="font-mono" style={{ color: 'var(--color-text-muted)' }}>
          {currentTime.toLocaleTimeString()}
        </span>
      </div>
    </footer>
  );
}

