import { useState, useCallback, useRef, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { ConfigPanel } from './components/ConfigPanel';
import { StatusMonitor } from './components/StatusMonitor';
import { UsbChatPanel } from './components/UsbChatPanel';
import { ChannelsPanel } from './components/ChannelsPanel';
import { ToastProvider, ToastContainer } from './components/Toast';
import { WifiScanner } from './components/WifiScanner';
import { SafeModeToggle } from './components/SafeModeToggle';
import { IdentityPanel } from './components/IdentityPanel';
import { ConfigImportExport } from './components/ConfigImportExport';
import { UsbPanel } from './components/UsbPanel';
import { UsbConnectDialog, type UsbPhase } from './components/UsbConnectDialog';
import { TopBarSettings } from './components/TopBarSettings';
import { Footer } from './components/Footer';
import { ConfirmQuit } from './components/ConfirmQuit';
import { Sidebar, type NavItem } from './components/Sidebar';
import { useToast } from './components/Toast';
import { useTheme } from './contexts/ThemeContext';
import { useAppInfo } from './hooks/useAppInfo';
import { bleDisconnect, bleScan } from './hooks/useBle';
import { useUsbAutoConnect } from './hooks/useUsbAutoConnect';
import { ConfigStorage } from './utils/storage';
import { useBleReconnect } from './hooks/useBleReconnect';
import type { BleDevice, ConnectionState } from './types';

type TabType = NavItem;

// Module-level "sticky" flag: once the USB link is established it survives
// component remounts within a page session (e.g. React StrictMode / dev HMR
// remounts), so the UI never re-shows the "connecting" process for an already
// connected backend — preventing a visible "repeatedly re-connecting" flash.
let usbLinkEstablished = false;

interface ImportedConfig {
  wifi_ssid?: string;
  llm_model?: string;
  hostname?: string;
}

function AppContent() {
  const { t } = useTranslation();
  const toast = useToast();
  const { theme } = useTheme();
  const appInfo = useAppInfo();
  const [connectionState, setConnectionState] = useState<ConnectionState>('disconnected');
  const [connectedDevice, setConnectedDevice] = useState<BleDevice | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<TabType>('chat');
  // HARDENING (audit-2026-08 M6): guard `setTimeout` + `setState` calls
  // against component unmount. Without this, a 3-second `setConnectionState`
  // after a failed connection attempt could call `setState` on an unmounted
  // component, triggering a React "Can't perform a React state update on an
  // unmounted component" warning and potential memory leaks.
  const isMountedRef = useRef(true);
  useEffect(() => {
    isMountedRef.current = true;
    return () => { isMountedRef.current = false; };
  }, []);
  const [pendingWifiSsid, setPendingWifiSsid] = useState<string | null>(null);
  const [importedConfig, setImportedConfig] = useState<ImportedConfig | null>(null);
  // Status-bar data: how many BLE devices the last scan found and when.
  const [deviceCount, setDeviceCount] = useState(0);
  const [lastScanTime, setLastScanTime] = useState<Date | null>(null);
  const [scanning, setScanning] = useState(false);
  // Whether the navigation sidebar is open. Persisted so the preference
  // survives restarts.
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(() => {
    if (typeof window !== 'undefined') {
      return localStorage.getItem('sidebarOpen') !== 'false';
    }
    return true;
  });
  useEffect(() => {
    localStorage.setItem('sidebarOpen', String(sidebarOpen));
  }, [sidebarOpen]);

  // Auto-scan USB ports and auto-connect on startup (no manual step).
  const usb = useUsbAutoConnect();
  // Connection-process dialog: shows scanning → connecting → success window.
  const [usbDialogOpen, setUsbDialogOpen] = useState(true);
  // Once the link is up, keep the phase "connected" and never re-show the
  // connect process, even across remounts within this page session.
  if (usb.connectedPath) usbLinkEstablished = true;
  const usbPhase: UsbPhase = usbLinkEstablished
    ? 'connected'
    : usb.connecting
      ? 'connecting'
      : usb.scanning
        ? 'scanning'
        : usb.error
          ? 'error'
          : 'none';

  // Auto-enter the conversation (Chat tab) once the startup USB auto-connect
  // succeeds. Guarded so a later manual reconnect doesn't yank the user away.
  const autoEnteredChat = useRef(false);
  useEffect(() => {
    if (autoEnteredChat.current) return;
    if (usb.connectedPath) {
      autoEnteredChat.current = true;
      setActiveTab('chat');
    }
  }, [usb.connectedPath]);

  // Auto-dismiss the "Connection Successful" window shortly after it appears
  // so the user lands straight in the chat view.
  useEffect(() => {
    if (usbPhase === 'connected' && usbDialogOpen) {
      const t = setTimeout(() => setUsbDialogOpen(false), 1500);
      return () => clearTimeout(t);
    }
  }, [usbPhase, usbDialogOpen]);

  // BLE reconnection hook
  const reconnect = useBleReconnect({
    maxRetries: 5,
    baseDelay: 1000,
    maxDelay: 30000,
    onConnected: () => {
      toast.success(t('toast.connectionRestored'));
    },
    onDisconnected: () => {
      toast.warning(t('toast.connectionLost'));
    },
    onError: (err) => {
      toast.error(err.message);
    },
  });

  const handleDisconnect = useCallback(async () => {
    reconnect.cancelReconnect();
    try {
      await bleDisconnect();
      setConnectedDevice(null);
      setConnectionState('disconnected');
    } catch (e) {
      console.error('Disconnect failed:', e);
    }
  }, [reconnect]);

  // Status-bar "Scan" action: discover nearby BLE devices and update the
  // device count + last-scan timestamp shown in the footer.
  const handleFooterScan = useCallback(async () => {
    setScanning(true);
    try {
      const found = await bleScan();
      setDeviceCount(found.length);
    } catch (e) {
      console.error('Status-bar scan failed:', e);
      setDeviceCount(0);
    } finally {
      setLastScanTime(new Date());
      setScanning(false);
    }
  }, []);

  const handleWifiNetworkSelect = useCallback((ssid: string) => {
    // Pre-fill the SSID in the Configuration tab so the user can review it
    // before saving the WiFi credentials to the device.
    setPendingWifiSsid(ssid);
    setActiveTab('config');
    toast.info(t('wifiScan.selected', { ssid }));
  }, [toast, t]);

  const handleConfigImport = useCallback((config: ImportedConfig) => {
    const deviceId = connectedDevice?.id;

    if (!deviceId) {
      toast.warning(t('configImport.notConnected'));
      return;
    }

    // Persist non-destructively (the exported file has no password) and pre-fill
    // the Configuration tab so the user can review before writing to the device.
    const patch: Partial<{ wifi_ssid: string; llm_model: string; hostname: string }> = {};
    if (config.wifi_ssid !== undefined) patch.wifi_ssid = config.wifi_ssid;
    if (config.llm_model !== undefined) patch.llm_model = config.llm_model;
    if (config.hostname !== undefined) patch.hostname = config.hostname;

    if (Object.keys(patch).length > 0) {
      ConfigStorage.saveConfig(deviceId, patch);
    }

    setImportedConfig(config);
    setActiveTab('config');
    toast.success(t('configImport.importSuccess'));
  }, [connectedDevice, toast, t]);

  const dismissError = useCallback(() => {
    setError(null);
  }, []);

  const renderTabContent = () => {
    const isConnected = connectionState === 'connected';
    const deviceId = connectedDevice?.id || null;

    switch (activeTab) {
      case 'config':
        return (
          <ConfigPanel
            isConnected={isConnected}
            deviceId={deviceId}
            prefillSsid={pendingWifiSsid}
            importedConfig={importedConfig}
          />
        );
      case 'chat':
        return <UsbChatPanel autoConnected={usb.connectedPath !== null} autoPath={usb.connectedPath} />;
      case 'channels':
        return <ChannelsPanel isConnected={isConnected} />;
      case 'status':
        return <StatusMonitor isConnected={isConnected} />;
      case 'advanced':
        return (
          <div className="h-full overflow-y-auto">
            <div className="grid grid-cols-1 xl:grid-cols-2 gap-4">
              <WifiScanner isConnected={isConnected} onSelectNetwork={handleWifiNetworkSelect} />
              <SafeModeToggle isConnected={isConnected} />
              <IdentityPanel isConnected={isConnected} />
              <ConfigImportExport isConnected={isConnected} onImport={handleConfigImport} />
            </div>
            {/* USB-serial transport: talk to the C61 over UART0 instead of BLE */}
            <div className="mt-4">
              <UsbPanel />
            </div>
          </div>
        );
      default:
        return <ConfigPanel isConnected={isConnected} />;
    }
  };

  // Get topbar style based on theme
  const getTopbarBg = () => {
    switch (theme) {
      case 'dark':
        return 'linear-gradient(135deg, #1a1a2e 0%, #16213e 100%)';
      case 'warm':
        return 'linear-gradient(135deg, #fef7ed 0%, #fde7d6 100%)';
      case 'coffee':
        return 'linear-gradient(135deg, #2a1810 0%, #3d2415 100%)';
      default:
        return 'linear-gradient(135deg, #3b82f6 0%, #2563eb 100%)';
    }
  };

  return (
    <div className="flex h-screen relative overflow-hidden" style={{ fontFamily: 'system-ui, -apple-system, sans-serif' }}>
      {/* Decorative ambient background for coffee theme */}
      {theme === 'coffee' && (
        <div
          className="absolute inset-0 pointer-events-none opacity-60"
          style={{
            background: 'radial-gradient(circle at 30% 0%, rgba(212, 165, 116, 0.08) 0%, transparent 50%), radial-gradient(circle at 70% 100%, rgba(232, 169, 76, 0.05) 0%, transparent 50%)',
            zIndex: 0
          }}
        />
      )}

      {/* Reconnecting Banner */}
      {reconnect.isReconnecting && (
        <div className="fixed top-0 left-0 right-0 z-50 flex items-center justify-center px-6 py-2 shadow-md" style={{
          backgroundColor: 'var(--color-warning-light)',
          borderBottom: '1px solid var(--color-warning)'
        }}>
          <span className="flex items-center gap-2 text-sm font-medium" style={{ color: 'var(--color-warning)' }}>
            <span className="w-4 h-4 border-2 rounded-full animate-spin" style={{
              borderColor: 'var(--color-warning)',
              borderTopColor: 'transparent'
            }} />
            {t('toast.reconnecting')} ({reconnect.retryCount}/{5})
          </span>
        </div>
      )}

      {/* Error Banner */}
      {error && (
        <div className="fixed top-0 left-0 right-0 z-50 flex items-center justify-between px-6 py-3 shadow-md" style={{
          backgroundColor: 'var(--color-error-light)',
          borderBottom: '1px solid var(--color-error)'
        }}>
          <span className="flex items-center gap-2 text-sm font-medium" style={{ color: 'var(--color-error)' }}>
            <span>⚠️</span>
            <span>{error}</span>
          </span>
          <button
            onClick={dismissError}
            className="text-lg hover:opacity-70 transition-opacity"
            style={{ color: 'var(--color-error)' }}
          >
            ×
          </button>
        </div>
      )}

      {/* Navigation Sidebar */}
      {sidebarOpen && (
        <Sidebar
          activeNav={activeTab}
          onNavChange={setActiveTab}
          onCollapse={() => setSidebarOpen(false)}
        />
      )}

      {/* Main Content Area */}
      <div className="flex-1 flex flex-col overflow-hidden relative" style={{ marginTop: reconnect.isReconnecting || error ? '40px' : 0 }}>
        {/* Top Bar: Connection Status + Settings */}
        <header
          className="flex items-center justify-between px-6 py-3 flex-shrink-0"
          style={{
            background: getTopbarBg(),
            borderBottom: '1px solid rgba(128, 128, 128, 0.2)'
          }}
        >
          <div className="flex items-center gap-3">
            {/* Expand button shown only while the sidebar is collapsed */}
            {!sidebarOpen && (
              <button
                onClick={() => setSidebarOpen(true)}
                title={t('nav.menu')}
                aria-label={t('nav.menu')}
                className="flex items-center justify-center w-9 h-9 rounded-lg bg-white/10 hover:bg-white/20 transition-all duration-200 hover:scale-105 text-lg"
              >
                ☰
              </button>
            )}
            {/* Connection Status */}
            <div
              className="flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-medium text-white"
              style={{
                backgroundColor: connectionState === 'connected'
                  ? 'rgba(34, 197, 94, 0.3)'
                  : connectionState === 'connecting'
                    ? 'rgba(234, 179, 8, 0.3)'
                    : 'rgba(255, 255, 255, 0.15)',
                border: '1px solid rgba(255, 255, 255, 0.2)'
              }}
            >
              <span
                className={`w-2 h-2 rounded-full ${connectionState === 'connected' ? 'animate-pulse' : ''}`}
                style={{
                  backgroundColor: connectionState === 'connected'
                    ? '#22c55e'
                    : connectionState === 'connecting'
                      ? '#eab308'
                      : '#9ca3af'
                }}
              />
              {connectionState === 'connected' && connectedDevice
                ? connectedDevice.name
                : connectionState === 'connecting'
                  ? t('status.connecting')
                  : connectionState === 'error'
                    ? t('status.error')
                    : t('status.disconnected')
              }
            </div>
            {connectionState === 'connected' && (
              <button
                onClick={handleDisconnect}
                className="px-3 py-1.5 text-xs font-medium rounded-full transition-all duration-200 hover:scale-105"
                style={{
                  backgroundColor: 'rgba(239, 68, 68, 0.2)',
                  color: 'white',
                  border: '1px solid rgba(239, 68, 68, 0.4)'
                }}
              >
                {t('status.disconnect')}
              </button>
            )}

            {/* USB auto-connect status */}
            <div
              className="flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-medium text-white"
              style={{
                backgroundColor: usb.connectedPath
                  ? 'rgba(34, 197, 94, 0.3)'
                  : usb.scanning || usb.connecting
                    ? 'rgba(234, 179, 8, 0.3)'
                    : 'rgba(255, 255, 255, 0.15)',
                border: '1px solid rgba(255, 255, 255, 0.2)'
              }}
              title={usb.error || undefined}
            >
              <span
                className={`w-2 h-2 rounded-full ${usb.connectedPath ? 'animate-pulse' : ''}`}
                style={{
                  backgroundColor: usb.connectedPath
                    ? '#22c55e'
                    : usb.scanning || usb.connecting
                      ? '#eab308'
                      : usb.error
                        ? '#ef4444'
                        : '#9ca3af'
                }}
              />
              {usb.connectedPath
                ? t('usb.connected', { path: usb.connectedPath })
                : usb.connecting
                  ? t('usb.connecting')
                  : usb.scanning
                    ? t('usb.scanning')
                    : usb.error
                      ? t('usb.failed')
                      : t('usb.noDevice')}
            </div>
          </div>

          <div className="flex items-center gap-3">
            <span
              className="text-xs font-mono text-white/70 hidden sm:inline"
            >
              v{appInfo.version}
            </span>
            {/* Theme switch + language dropdown in the top-right corner */}
            <TopBarSettings />
          </div>
        </header>

        {/* Content */}
        <main
          className="flex-1 overflow-y-auto p-6"
          style={{ backgroundColor: 'var(--color-bg)' }}
        >
          <div className="max-w-5xl mx-auto animate-fade-in">
            {renderTabContent()}
          </div>
        </main>

        {/* Status bar (bottom of the window) */}
        <Footer
          connectionState={connectionState}
          connectedDeviceName={connectedDevice?.name ?? null}
          deviceCount={deviceCount}
          lastScanTime={lastScanTime}
          onScan={handleFooterScan}
          scanning={scanning}
        />
      </div>

      {/* Toast Notifications */}
      <ToastContainer />

      {/* USB connection-process / success dialog */}
      <UsbConnectDialog
        open={usbDialogOpen && !usbLinkEstablished}
        phase={usbPhase}
        path={usb.connectedPath ?? usb.ports[0] ?? null}
        error={usb.error}
        onClose={() => setUsbDialogOpen(false)}
        onRetry={usb.autoConnect}
      />

      {/* Quit confirmation */}
      <ConfirmQuit />
    </div>
  );
}

function App() {
  return (
    <ToastProvider>
      <AppContent />
    </ToastProvider>
  );
}

export default App;
