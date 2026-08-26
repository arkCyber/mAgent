import { useState, useCallback, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useToast } from './Toast';
import { bleExecCommand } from '../hooks/useBle';
import { parseSafeMode } from '../utils/at';

interface SafeModeToggleProps {
  isConnected: boolean;
}

/**
 * Reads the device safe-mode flag (`AT+SAFEMODE?`) and toggles it
 * (`AT+SAFEMODE=0/1`). The BLE helper acknowledges writes even when it
 * cannot relay the notification payload, so the UI optimistically reflects
 * the new state after a successful write.
 */
export function SafeModeToggle({ isConnected }: SafeModeToggleProps) {
  const { t } = useTranslation();
  const toast = useToast();
  const [isEnabled, setIsEnabled] = useState(false);
  const [isLoading, setIsLoading] = useState(false);

  // Load current safe-mode state from the device
  const loadState = useCallback(async () => {
    if (!isConnected) return;

    setIsLoading(true);
    try {
      const result = await bleExecCommand('AT+SAFEMODE?');
      const parsed = parseSafeMode(result.message);
      if (parsed !== null) {
        setIsEnabled(parsed);
      }
    } catch (error) {
      console.error('Failed to read safe mode:', error);
    } finally {
      setIsLoading(false);
    }
  }, [isConnected]);

  useEffect(() => {
    if (isConnected) {
      loadState();
    } else {
      setIsEnabled(false);
    }
  }, [isConnected, loadState]);

  const handleToggle = useCallback(async () => {
    if (!isConnected) {
      toast.warning(t('safeMode.notConnected'));
      return;
    }

    const target = !isEnabled;
    setIsLoading(true);
    try {
      const result = await bleExecCommand(`AT+SAFEMODE=${target ? 1 : 0}`);
      if (!result.success) {
        throw new Error(result.message);
      }
      setIsEnabled(target);

      if (target) {
        toast.success(t('safeMode.enabled'));
      } else {
        toast.info(t('safeMode.disabled'));
      }
    } catch (error) {
      toast.error(t('safeMode.error'));
    } finally {
      setIsLoading(false);
    }
  }, [isConnected, isEnabled, toast, t]);

  return (
    <div className="rounded-xl p-5 border" style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}>
      <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
        <span className="text-xl">🛡️</span>
        <h3 className="font-semibold" style={{ color: 'var(--color-text)' }}>{t('safeMode.title')}</h3>
      </div>

      <div className="flex items-center justify-between">
        <div>
          <p className="font-medium text-sm">{t('safeMode.label')}</p>
          <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">
            {t('safeMode.description')}
          </p>
        </div>

        <button
          onClick={handleToggle}
          disabled={!isConnected || isLoading}
          className={`relative w-14 h-8 rounded-full transition-colors duration-300 focus:outline-none focus:ring-2 focus:ring-offset-2 ${
            isEnabled
              ? 'bg-green-500 focus:ring-green-500'
              : 'bg-gray-300 dark:bg-gray-600 focus:ring-gray-500'
          } disabled:opacity-50`}
          aria-pressed={isEnabled}
          aria-label={t('safeMode.toggle')}
        >
          <span
            className={`absolute top-1 left-1 w-6 h-6 bg-white rounded-full shadow-md transition-transform duration-300 ${
              isEnabled ? 'translate-x-6' : 'translate-x-0'
            }`}
          >
            {isLoading ? (
              <span className="absolute inset-0 flex items-center justify-center">
                <span className="w-3 h-3 border-2 border-gray-400/30 border-t-gray-600 rounded-full animate-spin" />
              </span>
            ) : (
              <span className="absolute inset-0 flex items-center justify-center text-xs">
                {isEnabled ? '🛡️' : '⚡'}
              </span>
            )}
          </span>
        </button>
      </div>

      {isEnabled && (
        <div className="mt-4 p-3 bg-green-50 dark:bg-green-900/20 rounded-lg">
          <p className="text-xs text-green-600 dark:text-green-400">
            {t('safeMode.activeWarning')}
          </p>
        </div>
      )}
    </div>
  );
}
