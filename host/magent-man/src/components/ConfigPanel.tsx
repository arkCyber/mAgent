import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import type { DeviceConfig } from '../types';
import { bleReadConfig, bleWriteWifi, bleWriteLlm, bleWriteHostname } from '../hooks/useBle';
import { ConfigStorage } from '../utils/storage';

interface ConfigPanelProps {
  isConnected: boolean;
  deviceId?: string | null;
  /** SSID selected via the WiFi scanner, pre-filled into the form for review. */
  prefillSsid?: string | null;
  /** Config imported from a file, pre-filled into the form for review. */
  importedConfig?: {
    wifi_ssid?: string;
    llm_model?: string;
    hostname?: string;
  } | null;
}

type SaveState = 'idle' | 'saving' | 'success' | 'error';

export function ConfigPanel({ isConnected, deviceId, prefillSsid, importedConfig }: ConfigPanelProps) {
  const { t } = useTranslation();
  const [config, setConfig] = useState<DeviceConfig | null>(null);
  const [loading, setLoading] = useState(false);

  // WiFi form state
  const [wifiSsid, setWifiSsid] = useState('');
  const [wifiPassword, setWifiPassword] = useState('');
  const [showWifiPassword, setShowWifiPassword] = useState(false);
  const [wifiSaveState, setWifiSaveState] = useState<SaveState>('idle');
  const [wifiMessage, setWifiMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  // LLM form state
  const [llmModel, setLlmModel] = useState('');
  const [llmApiKey, setLlmApiKey] = useState('');
  const [showApiKey, setShowApiKey] = useState(false);
  const [llmSaveState, setLlmSaveState] = useState<SaveState>('idle');
  const [llmMessage, setLlmMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  // Hostname form state
  const [hostname, setHostname] = useState('');
  const [hostnameSaveState, setHostnameSaveState] = useState<SaveState>('idle');
  const [hostnameMessage, setHostnameMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);

  // Load configuration from local storage or device
  const loadConfig = useCallback(async () => {
    if (!isConnected || !deviceId) return;

    setLoading(true);
    try {
      // First, try to load from local storage
      const storedConfig = ConfigStorage.getConfig(deviceId);

      // Then try to get current values from device
      const cfg = await bleReadConfig();
      setConfig(cfg);

      // Update local storage with device values (keeping any stored secrets)
      ConfigStorage.saveConfig(deviceId, {
        wifi_ssid: cfg.wifi_ssid,
        wifi_password: storedConfig?.wifi_password || null,
        llm_model: cfg.llm_model,
        llm_api_key: storedConfig?.llm_api_key || null,
        hostname: cfg.hostname,
      });

      // Prefer locally stored values but fall back to what the device reports.
      setWifiSsid(storedConfig?.wifi_ssid || cfg.wifi_ssid || '');
      setLlmModel(storedConfig?.llm_model || cfg.llm_model || '');
      setHostname(storedConfig?.hostname || cfg.hostname || '');
    } catch (e) {
      console.error('Failed to load config:', e);
      // On error, try to use local storage values
      const storedConfig = ConfigStorage.getConfig(deviceId);
      if (storedConfig) {
        if (storedConfig.wifi_ssid) setWifiSsid(storedConfig.wifi_ssid);
        if (storedConfig.llm_model) setLlmModel(storedConfig.llm_model);
        if (storedConfig.hostname) setHostname(storedConfig.hostname);
      }
    } finally {
      setLoading(false);
    }
  }, [isConnected, deviceId]);

  useEffect(() => {
    if (isConnected && deviceId) {
      loadConfig();
    } else {
      setConfig(null);
      setWifiSsid('');
      setWifiPassword('');
      setLlmModel('');
      setLlmApiKey('');
      setHostname('');
    }
  }, [isConnected, deviceId, loadConfig]);

  // Apply a network selected via the WiFi scanner for review.
  useEffect(() => {
    if (prefillSsid) {
      setWifiSsid(prefillSsid);
    }
  }, [prefillSsid]);

  // Apply an imported config for review before writing to the device.
  useEffect(() => {
    if (!importedConfig) return;
    if (importedConfig.wifi_ssid) setWifiSsid(importedConfig.wifi_ssid);
    if (importedConfig.llm_model) setLlmModel(importedConfig.llm_model);
    if (importedConfig.hostname) setHostname(importedConfig.hostname);
  }, [importedConfig]);

  const handleSaveWifi = useCallback(async () => {
    if (!wifiSsid.trim()) {
      setWifiMessage({ type: 'error', text: t('config.wifi.ssidRequired') });
      return;
    }

    setWifiSaveState('saving');
    setWifiMessage(null);

    try {
      const result = await bleWriteWifi(wifiSsid, wifiPassword);
      setWifiSaveState(result.success ? 'success' : 'error');
      setWifiMessage({
        type: result.success ? 'success' : 'error',
        text: result.message
      });

      if (result.success && deviceId) {
        // Save to local storage
        ConfigStorage.saveConfig(deviceId, {
          wifi_ssid: wifiSsid,
          wifi_password: wifiPassword || undefined,
        });
        setWifiPassword('');
        setTimeout(() => setWifiSaveState('idle'), 3000);
      }
    } catch (e) {
      setWifiSaveState('error');
      setWifiMessage({
        type: 'error',
        text: e instanceof Error ? e.message : t('config.saveFailed')
      });
    }
  }, [wifiSsid, wifiPassword, deviceId, t]);

  const handleSaveLlm = useCallback(async () => {
    if (!llmModel.trim()) {
      setLlmMessage({ type: 'error', text: t('config.llm.modelRequired') });
      return;
    }
    if (!llmApiKey.trim()) {
      setLlmMessage({ type: 'error', text: t('config.llm.apiKeyRequired') });
      return;
    }

    setLlmSaveState('saving');
    setLlmMessage(null);

    try {
      const result = await bleWriteLlm(llmModel, llmApiKey);
      setLlmSaveState(result.success ? 'success' : 'error');
      setLlmMessage({
        type: result.success ? 'success' : 'error',
        text: result.message
      });

      if (result.success && deviceId) {
        // Save to local storage (without exposing API key in full)
        ConfigStorage.saveConfig(deviceId, {
          llm_model: llmModel,
          llm_api_key: llmApiKey,
        });
        setTimeout(() => {
          setLlmSaveState('idle');
          setLlmMessage(null);
        }, 3000);
      }
    } catch (e) {
      setLlmSaveState('error');
      setLlmMessage({
        type: 'error',
        text: e instanceof Error ? e.message : t('config.saveFailed')
      });
    }
  }, [llmModel, llmApiKey, deviceId, t]);

  const handleSaveHostname = useCallback(async () => {
    if (!hostname.trim()) {
      setHostnameMessage({ type: 'error', text: t('config.hostname.required') });
      return;
    }

    setHostnameSaveState('saving');
    setHostnameMessage(null);

    try {
      const result = await bleWriteHostname(hostname);
      setHostnameSaveState(result.success ? 'success' : 'error');
      setHostnameMessage({
        type: result.success ? 'success' : 'error',
        text: result.message
      });

      if (result.success && deviceId) {
        // Save to local storage
        ConfigStorage.saveConfig(deviceId, {
          hostname: hostname,
        });
        setTimeout(() => setHostnameSaveState('idle'), 3000);
      }
    } catch (e) {
      setHostnameSaveState('error');
      setHostnameMessage({
        type: 'error',
        text: e instanceof Error ? e.message : t('config.saveFailed')
      });
    }
  }, [hostname, deviceId, t]);

  if (!isConnected) {
    return (
      <div className="config-panel">
        <div className="flex items-center justify-between mb-5">
          <h2 className="text-xl font-semibold">{t('config.title')}</h2>
        </div>
        <div className="flex flex-col items-center justify-center py-16 text-center bg-white dark:bg-gray-800 rounded-xl">
          <span className="text-5xl opacity-30">🔗</span>
          <h3 className="mt-4 text-lg font-medium">{t('config.notConnected')}</h3>
          <p className="mt-2 text-gray-500 dark:text-gray-400">{t('config.notConnectedHint')}</p>
        </div>
      </div>
    );
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-5">
        <h2 className="text-xl font-semibold">{t('config.title')}</h2>
        <button
          onClick={loadConfig}
          disabled={loading}
          className="w-9 h-9 flex items-center justify-center bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
        >
          <span className={loading ? 'animate-spin' : ''}>↻</span>
        </button>
      </div>

      {/* WiFi Settings Section */}
      <div className="bg-white dark:bg-gray-800 rounded-xl p-5 mb-4 shadow-sm">
        <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
          <span className="text-xl">📶</span>
          <h3 className="font-semibold">{t('config.wifi.title')}</h3>
        </div>

        {config?.wifi_ssid && (
          <div className="mb-4 p-3 bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400 rounded-lg text-sm">
            {t('config.currentConfig')} <strong className="font-mono">{config.wifi_ssid}</strong>
          </div>
        )}

        <div className="space-y-4">
          <div>
            <label className="block text-sm font-medium mb-2">{t('config.wifi.ssid')}</label>
            <input
              type="text"
              value={wifiSsid}
              onChange={(e) => setWifiSsid(e.target.value)}
              placeholder={t('config.wifi.ssidPlaceholder')}
              disabled={wifiSaveState === 'saving'}
              className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-60 transition-colors"
            />
          </div>

          <div>
            <label className="block text-sm font-medium mb-2">{t('config.wifi.password')}</label>
            <div className="relative">
              <input
                type={showWifiPassword ? 'text' : 'password'}
                value={wifiPassword}
                onChange={(e) => setWifiPassword(e.target.value)}
                placeholder={config?.wifi_password ? '••••••••' : t('config.wifi.passwordPlaceholder')}
                disabled={wifiSaveState === 'saving'}
                className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-60 transition-colors"
              />
              <button
                type="button"
                onClick={() => setShowWifiPassword(!showWifiPassword)}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
              >
                {showWifiPassword ? '🙈' : '👁️'}
              </button>
            </div>
          </div>
        </div>

        {wifiMessage && (
          <div className={`mt-4 p-3 rounded-lg text-sm ${
            wifiMessage.type === 'success'
              ? 'bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400'
              : 'bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400'
          }`}>
            {wifiMessage.type === 'success' ? '✓' : '⚠️'} {wifiMessage.text}
          </div>
        )}

        <button
          onClick={handleSaveWifi}
          disabled={wifiSaveState === 'saving'}
          className={`mt-4 w-full py-3 rounded-lg text-sm font-medium text-white transition-colors ${
            wifiSaveState === 'success'
              ? 'bg-green-500 hover:bg-green-600'
              : wifiSaveState === 'error'
              ? 'bg-red-500 hover:bg-red-600'
              : 'bg-blue-500 hover:bg-blue-600'
          } disabled:opacity-70`}
        >
          {wifiSaveState === 'saving' ? (
            <span className="flex items-center justify-center gap-2">
              <span className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
              {t('config.wifi.saving')}
            </span>
          ) : wifiSaveState === 'success' ? (
            `✓ ${t('config.wifi.saved')}`
          ) : (
            t('config.wifi.save')
          )}
        </button>
      </div>

      {/* Hostname Settings Section */}
      <div className="bg-white dark:bg-gray-800 rounded-xl p-5 mb-4 shadow-sm">
        <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
          <span className="text-xl">🌐</span>
          <h3 className="font-semibold">{t('config.hostname.title')}</h3>
        </div>

        {config?.hostname && (
          <div className="mb-4 p-3 bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400 rounded-lg text-sm">
            {t('config.currentConfig')} <strong className="font-mono">{config.hostname}</strong>
          </div>
        )}

        <div>
          <label className="block text-sm font-medium mb-2">{t('config.hostname.label')}</label>
          <input
            type="text"
            value={hostname}
            onChange={(e) => setHostname(e.target.value)}
            placeholder={t('config.hostname.placeholder')}
            disabled={hostnameSaveState === 'saving'}
            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-60 transition-colors"
          />
          <span className="block mt-1 text-xs text-gray-500">{t('config.hostname.hint')}</span>
        </div>

        {hostnameMessage && (
          <div className={`mt-4 p-3 rounded-lg text-sm ${
            hostnameMessage.type === 'success'
              ? 'bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400'
              : 'bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400'
          }`}>
            {hostnameMessage.type === 'success' ? '✓' : '⚠️'} {hostnameMessage.text}
          </div>
        )}

        <button
          onClick={handleSaveHostname}
          disabled={hostnameSaveState === 'saving'}
          className={`mt-4 w-full py-3 rounded-lg text-sm font-medium text-white transition-colors ${
            hostnameSaveState === 'success'
              ? 'bg-green-500 hover:bg-green-600'
              : hostnameSaveState === 'error'
              ? 'bg-red-500 hover:bg-red-600'
              : 'bg-blue-500 hover:bg-blue-600'
          } disabled:opacity-70`}
        >
          {hostnameSaveState === 'saving' ? (
            <span className="flex items-center justify-center gap-2">
              <span className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
              {t('config.hostname.saving')}
            </span>
          ) : hostnameSaveState === 'success' ? (
            `✓ ${t('config.hostname.saved')}`
          ) : (
            t('config.hostname.save')
          )}
        </button>
      </div>

      {/* LLM Settings Section */}
      <div className="bg-white dark:bg-gray-800 rounded-xl p-5 shadow-sm">
        <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
          <span className="text-xl">🤖</span>
          <h3 className="font-semibold">{t('config.llm.title')}</h3>
        </div>

        {config?.llm_model && (
          <div className="mb-4 p-3 bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400 rounded-lg text-sm">
            {t('config.currentConfig')} <strong className="font-mono">{config.llm_model}</strong>
          </div>
        )}

        <div className="space-y-4">
          <div>
            <label className="block text-sm font-medium mb-2">{t('config.llm.model')}</label>
            <input
              type="text"
              value={llmModel}
              onChange={(e) => setLlmModel(e.target.value)}
              placeholder={t('config.llm.modelPlaceholder')}
              disabled={llmSaveState === 'saving'}
              className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-60 transition-colors"
            />
            <span className="block mt-1 text-xs text-gray-500">{t('config.llm.modelHint')}</span>
          </div>

          <div>
            <label className="block text-sm font-medium mb-2">{t('config.llm.apiKey')}</label>
            <div className="relative">
              <input
                type={showApiKey ? 'text' : 'password'}
                value={llmApiKey}
                onChange={(e) => setLlmApiKey(e.target.value)}
                placeholder={config?.llm_api_key ? '••••••••' : t('config.llm.apiKeyPlaceholder')}
                disabled={llmSaveState === 'saving'}
                className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-60 transition-colors"
              />
              <button
                type="button"
                onClick={() => setShowApiKey(!showApiKey)}
                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
              >
                {showApiKey ? '🙈' : '👁️'}
              </button>
            </div>
            <span className="block mt-1 text-xs text-gray-500">{t('config.llm.apiKeyHint')}</span>
          </div>
        </div>

        {llmMessage && (
          <div className={`mt-4 p-3 rounded-lg text-sm ${
            llmMessage.type === 'success'
              ? 'bg-green-50 dark:bg-green-900/20 text-green-600 dark:text-green-400'
              : 'bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400'
          }`}>
            {llmMessage.type === 'success' ? '✓' : '⚠️'} {llmMessage.text}
          </div>
        )}

        <button
          onClick={handleSaveLlm}
          disabled={llmSaveState === 'saving'}
          className={`mt-4 w-full py-3 rounded-lg text-sm font-medium text-white transition-colors ${
            llmSaveState === 'success'
              ? 'bg-green-500 hover:bg-green-600'
              : llmSaveState === 'error'
              ? 'bg-red-500 hover:bg-red-600'
              : 'bg-blue-500 hover:bg-blue-600'
          } disabled:opacity-70`}
        >
          {llmSaveState === 'saving' ? (
            <span className="flex items-center justify-center gap-2">
              <span className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
              {t('config.llm.saving')}
            </span>
          ) : llmSaveState === 'success' ? (
            `✓ ${t('config.llm.saved')}`
          ) : (
            t('config.llm.save')
          )}
        </button>
      </div>
    </div>
  );
}
