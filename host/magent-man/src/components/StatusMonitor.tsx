import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import {
  bleGetDeviceInfo,
  bleGetWifiStatus,
  bleGetLogs,
  bleDiagnostics,
  bleReboot,
  type DeviceInfoResponse,
  type WifiStatusResponse,
  type DiagnosticsResponse
} from '../hooks/useBle';

interface StatusMonitorProps {
  isConnected: boolean;
}

export function StatusMonitor({ isConnected }: StatusMonitorProps) {
  const { t } = useTranslation();
  const [deviceInfo, setDeviceInfo] = useState<DeviceInfoResponse | null>(null);
  const [wifiStatus, setWifiStatus] = useState<WifiStatusResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdate, setLastUpdate] = useState<Date | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [diagnostics, setDiagnostics] = useState<DiagnosticsResponse | null>(null);
  const [showLogsModal, setShowLogsModal] = useState(false);
  const [showDiagnosticsModal, setShowDiagnosticsModal] = useState(false);

  const fetchStatus = useCallback(async () => {
    if (!isConnected) return;

    setLoading(true);
    setError(null);

    try {
      const [info, wifi] = await Promise.all([
        bleGetDeviceInfo(),
        bleGetWifiStatus(),
      ]);
      setDeviceInfo(info);
      setWifiStatus(wifi);
      setLastUpdate(new Date());
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to fetch status');
    } finally {
      setLoading(false);
    }
  }, [isConnected]);

  const fetchLogs = useCallback(async () => {
    if (!isConnected) return;

    setLoading(true);
    try {
      const result = await bleGetLogs(100);
      if (result.success) {
        setLogs(result.logs);
        setShowLogsModal(true);
      }
    } catch (e) {
      console.error('Failed to fetch logs:', e);
    } finally {
      setLoading(false);
    }
  }, [isConnected]);

  const fetchDiagnostics = useCallback(async () => {
    if (!isConnected) return;

    setLoading(true);
    try {
      const result = await bleDiagnostics();
      if (result.success) {
        setDiagnostics(result);
        setShowDiagnosticsModal(true);
      }
    } catch (e) {
      console.error('Failed to run diagnostics:', e);
    } finally {
      setLoading(false);
    }
  }, [isConnected]);

  const handleReboot = useCallback(async () => {
    if (!isConnected) return;

    if (!window.confirm(t('monitor.reboot.confirm'))) return;

    setLoading(true);
    try {
      await bleReboot();
      alert(t('monitor.reboot.success'));
    } catch (e) {
      console.error('Reboot failed:', e);
    } finally {
      setLoading(false);
    }
  }, [isConnected, t]);

  useEffect(() => {
    if (!isConnected) {
      setDeviceInfo(null);
      setWifiStatus(null);
      setError(null);
      return;
    }

    fetchStatus();

    const interval = setInterval(fetchStatus, 5000);
    return () => clearInterval(interval);
  }, [isConnected, fetchStatus]);

  const getWifiStateName = (state: number) => {
    const states: Record<number, string> = {
      0: t('monitor.wifi.state.idle'),
      1: t('monitor.wifi.state.connecting'),
      3: t('monitor.wifi.state.associated'),
      4: t('monitor.wifi.state.disconnected'),
      5: t('monitor.wifi.state.connected'),
    };
    return states[state] || 'Unknown';
  };

  const getWifiStateColor = (state: number) => {
    const colors: Record<number, string> = {
      0: 'bg-gray-500',
      1: 'bg-yellow-500',
      3: 'bg-blue-500',
      4: 'bg-red-500',
      5: 'bg-green-500',
    };
    return colors[state] || 'bg-gray-500';
  };

  const formatUptime = (ms: number) => {
    const seconds = Math.floor(ms / 1000);
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);
    const days = Math.floor(hours / 24);

    if (days > 0) return `${days}d ${hours % 24}h`;
    if (hours > 0) return `${hours}h ${minutes % 60}m`;
    if (minutes > 0) return `${minutes}m ${seconds % 60}s`;
    return `${seconds}s`;
  };

  const formatMemory = (bytes: number) => {
    if (bytes >= 1024 * 1024) {
      return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    }
    return `${Math.round(bytes / 1024)} KB`;
  };

  const getMemoryPercent = () => {
    // HARDENING (audit-2026-08 M2): `!deviceInfo.memory_total` catches
    // both `undefined`/`null` (initial state) and the zero-value case.
    // Previously `=== 0` missed the undefined path, returning NaN%.
    if (!deviceInfo || !deviceInfo.memory_total) return 0;
    return Math.round((deviceInfo.memory_free / deviceInfo.memory_total) * 100);
  };

  const getSignalBars = (rssi: number) => {
    if (rssi > -50) return 4;
    if (rssi > -60) return 3;
    if (rssi > -70) return 2;
    if (rssi > -80) return 1;
    return 0;
  };

  if (!isConnected) {
    return (
      <div>
        <div className="flex items-center justify-between mb-5">
          <h2 className="text-xl font-semibold">{t('monitor.title')}</h2>
        </div>
        <div className="flex flex-col items-center justify-center py-16 text-center bg-white dark:bg-gray-800 rounded-xl">
          <span className="text-5xl opacity-30">📊</span>
          <h3 className="mt-4 text-lg font-medium">{t('monitor.notConnected')}</h3>
          <p className="mt-2 text-gray-500 dark:text-gray-400">{t('monitor.notConnectedHint')}</p>
        </div>
      </div>
    );
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-5">
        <h2 className="text-xl font-semibold">{t('monitor.title')}</h2>
        <div className="flex items-center gap-3">
          {lastUpdate && (
            <span className="text-xs text-gray-500 dark:text-gray-400">
              {t('monitor.lastUpdate', { time: lastUpdate.toLocaleTimeString() })}
            </span>
          )}
          <button
            onClick={fetchStatus}
            disabled={loading}
            className="w-9 h-9 flex items-center justify-center bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
          >
            <span className={loading ? 'animate-spin' : ''}>↻</span>
          </button>
        </div>
      </div>

      {error && (
        <div className="flex items-center gap-2 p-3 mb-4 bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 rounded-lg text-sm">
          <span>⚠️</span> {error}
        </div>
      )}

      {/* Device Info Card */}
      <div className="bg-white dark:bg-gray-800 rounded-xl p-5 mb-4 shadow-sm">
        <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
          <span className="text-xl">💻</span>
          <h3 className="font-semibold">{t('monitor.device.title')}</h3>
        </div>

        <div className="grid grid-cols-2 gap-4 mb-4">
          <div>
            <span className="block text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
              {t('monitor.device.version')}
            </span>
            <span className="text-sm font-medium">
              v{deviceInfo ? `${deviceInfo.version_major}.${deviceInfo.version_minor}.${deviceInfo.version_patch}` : '--'}
            </span>
          </div>
          <div>
            <span className="block text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
              {t('monitor.device.chip')}
            </span>
            <span className="text-sm font-medium">
              {deviceInfo?.chip_model?.trim() || 'ESP32-C61'}
            </span>
          </div>
          <div>
            <span className="block text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
              {t('monitor.device.uptime')}
            </span>
            <span className="text-sm font-medium">
              {deviceInfo ? formatUptime(deviceInfo.uptime_ms) : '--'}
            </span>
          </div>
          <div>
            <span className="block text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
              {t('monitor.device.memory')}
            </span>
            <span className="text-sm font-medium">
              {deviceInfo ? formatMemory(deviceInfo.memory_free) : '--'}
            </span>
          </div>
        </div>

        {/* Memory Progress Bar */}
        {deviceInfo && (
          <div>
            <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400 mb-2">
              <span>{t('monitor.device.heapUsage')}</span>
              <span>{getMemoryPercent()}% {t('monitor.device.free')}</span>
            </div>
            <div className="h-2 bg-gray-200 dark:bg-gray-700 rounded-full overflow-hidden">
              <div
                className="h-full bg-gradient-to-r from-blue-500 to-green-500 rounded-full transition-all duration-500"
                style={{ width: `${getMemoryPercent()}%` }}
              />
            </div>
          </div>
        )}
      </div>

      {/* WiFi Status Card */}
      <div className="bg-white dark:bg-gray-800 rounded-xl p-5 mb-4 shadow-sm">
        <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
          <span className="text-xl">📶</span>
          <h3 className="flex-1 font-semibold">{t('monitor.wifi.title')}</h3>
          <span className={`px-2 py-1 rounded-full text-xs font-medium text-white ${getWifiStateColor(wifiStatus?.state || 4)}`}>
            {getWifiStateName(wifiStatus?.state || 4)}
          </span>
        </div>

        <div className="grid grid-cols-2 gap-4 mb-4">
          <div>
            <span className="block text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
              {t('monitor.wifi.network')}
            </span>
            <span className="text-sm font-medium">
              {wifiStatus?.ssid || t('monitor.wifi.notConnected')}
            </span>
          </div>
          <div>
            <span className="block text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide mb-1">
              {t('monitor.wifi.ipAddress')}
            </span>
            <span className="text-sm font-medium font-mono">
              {wifiStatus?.ip_addr || '--'}
            </span>
          </div>
        </div>

        {/* Signal Strength */}
        <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
          <div className="flex justify-between text-xs text-gray-500 dark:text-gray-400 mb-2">
            <span>{t('monitor.wifi.signal')}</span>
            <span className="font-mono">{wifiStatus?.rssi || 0} dBm</span>
          </div>
          <div className="flex items-end gap-1 h-6">
            {[1, 2, 3, 4].map((bar) => (
              <div
                key={bar}
                className={`w-6 rounded-sm transition-colors ${
                  bar <= getSignalBars(wifiStatus?.rssi || -100)
                    ? getWifiStateColor(5)
                    : 'bg-gray-200 dark:bg-gray-600'
                }`}
                style={{ height: `${bar * 6}px` }}
              />
            ))}
          </div>
        </div>
      </div>

      {/* Quick Actions */}
      <div className="grid grid-cols-3 gap-3">
        <button
          onClick={fetchStatus}
          disabled={loading}
          className="flex flex-col items-center gap-2 p-4 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl hover:border-blue-500 hover:bg-blue-50 dark:hover:bg-blue-900/20 transition-colors"
        >
          <span className="text-2xl">🔄</span>
          <span className="text-xs">{t('monitor.actions.refresh')}</span>
        </button>
        <button
          onClick={fetchLogs}
          disabled={loading}
          className="flex flex-col items-center gap-2 p-4 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl hover:border-blue-500 hover:bg-blue-50 dark:hover:bg-blue-900/20 transition-colors"
        >
          <span className="text-2xl">📋</span>
          <span className="text-xs">{t('monitor.actions.logs')}</span>
        </button>
        <button
          onClick={fetchDiagnostics}
          disabled={loading}
          className="flex flex-col items-center gap-2 p-4 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-xl hover:border-blue-500 hover:bg-blue-50 dark:hover:bg-blue-900/20 transition-colors"
        >
          <span className="text-2xl">🔧</span>
          <span className="text-xs">{t('monitor.actions.diagnostics')}</span>
        </button>
      </div>

      {/* Reboot Button */}
      <div className="mt-3">
        <button
          onClick={handleReboot}
          disabled={loading}
          className="w-full py-3 rounded-xl text-sm font-medium text-white bg-red-500 hover:bg-red-600 transition-colors disabled:opacity-50"
        >
          {loading ? (
            <span className="flex items-center justify-center gap-2">
              <span className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
              {t('monitor.reboot.button')}
            </span>
          ) : (
            `🔄 ${t('monitor.reboot.button')}`
          )}
        </button>
      </div>

      {/* Logs Modal */}
      {showLogsModal && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-white dark:bg-gray-800 rounded-2xl p-6 w-full max-w-2xl mx-4 max-h-[80vh] flex flex-col">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold">{t('monitor.logs.title')}</h3>
              <button
                onClick={() => setShowLogsModal(false)}
                className="w-8 h-8 flex items-center justify-center hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
              >
                ×
              </button>
            </div>
            <div className="flex-1 overflow-y-auto bg-gray-900 dark:bg-gray-900 rounded-lg p-4 font-mono text-sm text-green-400">
              {logs.map((log, index) => (
                <div key={index} className="mb-1">{log}</div>
              ))}
            </div>
            <button
              onClick={() => setShowLogsModal(false)}
              className="mt-4 px-4 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg text-sm font-medium transition-colors"
            >
              {t('common.close')}
            </button>
          </div>
        </div>
      )}

      {/* Diagnostics Modal */}
      {showDiagnosticsModal && diagnostics && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-white dark:bg-gray-800 rounded-2xl p-6 w-full max-w-md mx-4">
            <div className="flex items-center justify-between mb-4">
              <h3 className="text-lg font-semibold">{t('monitor.diagnostics.title')}</h3>
              <button
                onClick={() => setShowDiagnosticsModal(false)}
                className="w-8 h-8 flex items-center justify-center hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
              >
                ×
              </button>
            </div>
            <div className="space-y-4">
              {diagnostics.device && (
                <div className="bg-blue-50 dark:bg-blue-900/20 rounded-lg p-4">
                  <h4 className="font-medium text-blue-600 dark:text-blue-400 mb-2">Device</h4>
                  <div className="grid grid-cols-2 gap-2 text-sm">
                    <span className="text-gray-500">Version:</span>
                    <span>{diagnostics.device.version}</span>
                    <span className="text-gray-500">Chip:</span>
                    <span>{diagnostics.device.chip_model}</span>
                  </div>
                </div>
              )}
              {diagnostics.wifi && (
                <div className="bg-green-50 dark:bg-green-900/20 rounded-lg p-4">
                  <h4 className="font-medium text-green-600 dark:text-green-400 mb-2">WiFi</h4>
                  <div className="grid grid-cols-2 gap-2 text-sm">
                    <span className="text-gray-500">State:</span>
                    <span>{diagnostics.wifi.state}</span>
                    <span className="text-gray-500">RSSI:</span>
                    <span>{diagnostics.wifi.rssi} dBm</span>
                  </div>
                </div>
              )}
              {diagnostics.memory && (
                <div className="bg-purple-50 dark:bg-purple-900/20 rounded-lg p-4">
                  <h4 className="font-medium text-purple-600 dark:text-purple-400 mb-2">Memory</h4>
                  <div className="grid grid-cols-2 gap-2 text-sm">
                    <span className="text-gray-500">Free:</span>
                    <span>{(diagnostics.memory.free_bytes / 1024).toFixed(1)} KB</span>
                    <span className="text-gray-500">Usage:</span>
                    <span>{diagnostics.memory.usage_percent.toFixed(1)}%</span>
                  </div>
                </div>
              )}
            </div>
            <button
              onClick={() => setShowDiagnosticsModal(false)}
              className="mt-6 w-full px-4 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg text-sm font-medium transition-colors"
            >
              {t('common.close')}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
