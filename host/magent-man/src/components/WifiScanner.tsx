import { useState, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { useToast } from './Toast';
import { bleExecCommand } from '../hooks/useBle';
import { parseCwLap } from '../utils/at';

interface WifiAccessPoint {
  ssid: string;
  rssi: number;
  auth: number;
  channel: number;
}

interface WifiScannerProps {
  isConnected: boolean;
  onSelectNetwork: (ssid: string) => void;
}

type ScanState = 'idle' | 'scanning' | 'success' | 'error';

/**
 * Triggers a real device scan via `AT+CWLAP`. The firmware v0.2 replies with
 * `+CWLAP:scan-started` and the access-point table is emitted asynchronously,
 * so the list is populated from any table rows the helper relays and, when the
 * table is not available on this connection, the user is informed the scan was
 * started.
 */
export function WifiScanner({ isConnected, onSelectNetwork }: WifiScannerProps) {
  const { t } = useTranslation();
  const toast = useToast();
  const [scanState, setScanState] = useState<ScanState>('idle');
  const [networks, setNetworks] = useState<WifiAccessPoint[]>([]);
  const [selectedSsid, setSelectedSsid] = useState<string>('');

  const handleScan = useCallback(async () => {
    if (!isConnected) {
      toast.warning(t('wifiScan.notConnected'));
      return;
    }

    setScanState('scanning');
    setNetworks([]);

    try {
      const result = await bleExecCommand('AT+CWLAP');

      if (!result.success) {
        throw new Error(result.message);
      }

      // Parse `+CWLAP:(auth,"ssid",rssi,channel)` rows if the helper relayed them.
      const parsed = parseCwLap(result.message);

      if (parsed.length > 0) {
        setNetworks(parsed);
        setScanState('success');
        toast.success(t('wifiScan.found', { count: parsed.length }));
      } else {
        // v0.2 firmware starts a background scan; the table is emitted later
        // and may not be relayed. Report the scan was started.
        setScanState('success');
        toast.info(t('wifiScan.started'));
      }
    } catch (error) {
      setScanState('error');
      toast.error(t('wifiScan.failed'));
    }
  }, [isConnected, toast, t]);

  const handleSelect = useCallback((network: WifiAccessPoint) => {
    setSelectedSsid(network.ssid);
    onSelectNetwork(network.ssid);
  }, [onSelectNetwork]);

  const getSignalStrength = (rssi: number): { label: string; bars: number; color: string } => {
    if (rssi >= -50) return { label: t('wifiScan.excellent'), bars: 4, color: 'text-green-500' };
    if (rssi >= -60) return { label: t('wifiScan.good'), bars: 3, color: 'text-green-500' };
    if (rssi >= -70) return { label: t('wifiScan.fair'), bars: 2, color: 'text-yellow-500' };
    return { label: t('wifiScan.weak'), bars: 1, color: 'text-red-500' };
  };

  const getAuthLabel = (auth: number): string => {
    switch (auth) {
      case 0: return 'Open';
      case 1: return 'WEP';
      case 2: return 'WPA';
      case 3: return 'WPA2';
      case 4: return 'WPA2/WPA3';
      default: return 'Unknown';
    }
  };

  return (
    <div className="rounded-xl p-5 border" style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}>
      <div className="flex items-center justify-between mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
        <div className="flex items-center gap-2">
          <span className="text-xl">📡</span>
          <h3 className="font-semibold" style={{ color: 'var(--color-text)' }}>{t('wifiScan.title')}</h3>
        </div>
        <button
          onClick={handleScan}
          disabled={scanState === 'scanning' || !isConnected}
          className="flex items-center gap-2 px-4 py-2 bg-blue-500 hover:bg-blue-600 disabled:bg-gray-300 dark:disabled:bg-gray-600 text-white text-sm font-medium rounded-lg transition-colors"
        >
          {scanState === 'scanning' ? (
            <>
              <span className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
              {t('wifiScan.scanning')}
            </>
          ) : (
            <>
              <span>🔍</span>
              {t('wifiScan.scan')}
            </>
          )}
        </button>
      </div>

      {!isConnected && (
        <div className="text-center py-8 text-gray-500 dark:text-gray-400">
          <span className="text-3xl opacity-50">📶</span>
          <p className="mt-2 text-sm">{t('wifiScan.notConnected')}</p>
        </div>
      )}

      {isConnected && networks.length === 0 && scanState === 'idle' && (
        <div className="text-center py-8 text-gray-500 dark:text-gray-400">
          <span className="text-3xl opacity-50">📡</span>
          <p className="mt-2 text-sm">{t('wifiScan.hint')}</p>
        </div>
      )}

      {networks.length > 0 && (
        <div className="space-y-2 max-h-80 overflow-y-auto">
          {networks
            .sort((a, b) => a.rssi - b.rssi)
            .map((network, index) => {
              const signal = getSignalStrength(network.rssi);
              const isSelected = selectedSsid === network.ssid;

              return (
                <button
                  key={index}
                  onClick={() => handleSelect(network)}
                  className={`w-full flex items-center gap-4 p-3 rounded-lg transition-colors ${
                    isSelected
                      ? 'bg-blue-50 dark:bg-blue-900/30 border-2 border-blue-500'
                      : 'bg-gray-50 dark:bg-gray-700/50 hover:bg-gray-100 dark:hover:bg-gray-700 border-2 border-transparent'
                  }`}
                >
                  {/* Signal Strength Bars */}
                  <div className="flex items-end gap-0.5 h-6">
                    {[1, 2, 3, 4].map((bar) => (
                      <div
                        key={bar}
                        className={`w-1 rounded-sm transition-colors ${
                          bar <= signal.bars
                            ? signal.color
                            : 'bg-gray-300 dark:bg-gray-600'
                        }`}
                        style={{ height: `${bar * 4 + 4}px` }}
                      />
                    ))}
                  </div>

                  {/* Network Info */}
                  <div className="flex-1 text-left">
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm">{network.ssid || t('wifiScan.hidden')}</span>
                      {network.auth === 0 && (
                        <span className="text-xs px-1.5 py-0.5 bg-gray-200 dark:bg-gray-600 rounded">
                          Open
                        </span>
                      )}
                    </div>
                    <div className="flex items-center gap-2 mt-1 text-xs text-gray-500 dark:text-gray-400">
                      <span>{getAuthLabel(network.auth)}</span>
                      <span>•</span>
                      <span>Ch {network.channel}</span>
                      <span>•</span>
                      <span>{signal.label}</span>
                    </div>
                  </div>

                  {/* RSSI Value */}
                  <span className={`text-xs font-mono ${signal.color}`}>
                    {network.rssi} dBm
                  </span>
                </button>
              );
            })}
        </div>
      )}

      {scanState === 'error' && (
        <div className="mt-4 p-3 bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 rounded-lg text-sm text-center">
          {t('wifiScan.error')}
        </div>
      )}
    </div>
  );
}
