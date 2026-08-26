import { useState, useCallback, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useToast } from './Toast';
import { bleExecCommand } from '../hooks/useBle';
import { parseIdentityPublicKey, deriveAddress } from '../utils/at';

interface IdentityInfo {
  publicKey: string;
  address: string;
  chainId: number;
  rotatedAt: number | null;
}

interface IdentityPanelProps {
  isConnected: boolean;
}

/**
 * Reads the device Ed25519 identity (`AT+IDENT?`) and rotates it
 * (`AT+IDENTROT`). The BLE helper may not relay the raw notification payload,
 * so parsing is best-effort and a manual refresh is offered when the value
 * cannot be decoded on this connection.
 */
export function IdentityPanel({ isConnected }: IdentityPanelProps) {
  const { t } = useTranslation();
  const toast = useToast();
  const [identity, setIdentity] = useState<IdentityInfo | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [isRotating, setIsRotating] = useState(false);

  const loadIdentity = useCallback(async () => {
    if (!isConnected) return;

    setIsLoading(true);
    try {
      const result = await bleExecCommand('AT+IDENT?');
      const publicKey = parseIdentityPublicKey(result.message);

      if (publicKey) {
        setIdentity({
          publicKey: `${publicKey.slice(0, 10)}...${publicKey.slice(-4)}`,
          address: deriveAddress(publicKey),
          chainId: 1,
          rotatedAt: null,
        });
      } else {
        setIdentity(null);
      }
    } catch (error) {
      console.error('Failed to load identity:', error);
      toast.error(t('identity.loadError'));
    } finally {
      setIsLoading(false);
    }
  }, [isConnected, toast, t]);

  useEffect(() => {
    if (isConnected) {
      loadIdentity();
    }
  }, [isConnected, loadIdentity]);

  const handleRotate = useCallback(async () => {
    if (!isConnected) {
      toast.warning(t('identity.notConnected'));
      return;
    }

    setIsRotating(true);
    try {
      const result = await bleExecCommand('AT+IDENTROT');
      if (!result.success) {
        throw new Error(result.message);
      }

      // The new key is usually emitted as a notification the helper can't
      // relay here, so fall back to a refresh of the reported value.
      const publicKey = parseIdentityPublicKey(result.message);

      setIdentity((prev) =>
        publicKey
          ? {
              publicKey: `${publicKey.slice(0, 10)}...${publicKey.slice(-4)}`,
              address: deriveAddress(publicKey),
              chainId: prev?.chainId ?? 1,
              rotatedAt: Date.now(),
            }
          : prev
              ? { ...prev, rotatedAt: Date.now() }
              : prev
      );

      toast.success(t('identity.rotated'));
      loadIdentity();
    } catch (error) {
      toast.error(t('identity.rotateError'));
    } finally {
      setIsRotating(false);
    }
  }, [isConnected, toast, t, loadIdentity]);

  const formatDate = (timestamp: number | null): string => {
    if (!timestamp) return t('identity.never');
    return new Date(timestamp).toLocaleString();
  };

  if (!isConnected) {
    return (
      <div className="rounded-xl p-5 border" style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}>
        <div className="flex items-center gap-2 mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
          <span className="text-xl">🔑</span>
          <h3 className="font-semibold" style={{ color: 'var(--color-text)' }}>{t('identity.title')}</h3>
        </div>
        <div className="text-center py-8 text-gray-500 dark:text-gray-400">
          <span className="text-3xl opacity-50">🔑</span>
          <p className="mt-2 text-sm">{t('identity.notConnected')}</p>
        </div>
      </div>
    );
  }

  return (
    <div className="rounded-xl p-5 border" style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}>
      <div className="flex items-center justify-between mb-4 pb-3 border-b border-gray-200 dark:border-gray-700">
        <div className="flex items-center gap-2">
          <span className="text-xl">🔑</span>
          <h3 className="font-semibold" style={{ color: 'var(--color-text)' }}>{t('identity.title')}</h3>
        </div>
        <button
          onClick={loadIdentity}
          disabled={isLoading}
          className="w-8 h-8 flex items-center justify-center bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
        >
          <span className={isLoading ? 'animate-spin' : ''}>↻</span>
        </button>
      </div>

      {isLoading ? (
        <div className="flex items-center justify-center py-8">
          <span className="w-6 h-6 border-2 border-blue-500/30 border-t-blue-500 rounded-full animate-spin" />
        </div>
      ) : identity ? (
        <div className="space-y-4">
          {/* Public Key */}
          <div>
            <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
              {t('identity.publicKey')}
            </label>
            <div className="flex items-center gap-2">
              <code className="flex-1 px-3 py-2 bg-gray-100 dark:bg-gray-900 rounded-lg text-sm font-mono break-all">
                {identity.publicKey}
              </code>
              <button
                onClick={() => {
                  navigator.clipboard.writeText(identity.publicKey);
                  toast.success(t('identity.copied'));
                }}
                className="flex-shrink-0 w-8 h-8 flex items-center justify-center bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                title={t('identity.copy')}
              >
                📋
              </button>
            </div>
          </div>

          {/* Address */}
          <div>
            <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
              {t('identity.address')}
            </label>
            <div className="flex items-center gap-2">
              <code className="flex-1 px-3 py-2 bg-gray-100 dark:bg-gray-900 rounded-lg text-sm font-mono break-all">
                {identity.address}
              </code>
              <button
                onClick={() => {
                  navigator.clipboard.writeText(identity.address);
                  toast.success(t('identity.copied'));
                }}
                className="flex-shrink-0 w-8 h-8 flex items-center justify-center bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                title={t('identity.copy')}
              >
                📋
              </button>
            </div>
          </div>

          {/* Chain ID */}
          <div className="grid grid-cols-2 gap-4">
            <div>
              <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                {t('identity.chainId')}
              </label>
              <p className="px-3 py-2 bg-gray-100 dark:bg-gray-900 rounded-lg text-sm font-medium">
                {identity.chainId}
              </p>
            </div>
            <div>
              <label className="block text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                {t('identity.rotatedAt')}
              </label>
              <p className="px-3 py-2 bg-gray-100 dark:bg-gray-900 rounded-lg text-sm">
                {formatDate(identity.rotatedAt)}
              </p>
            </div>
          </div>

          {/* Rotate Button */}
          <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
            <button
              onClick={handleRotate}
              disabled={isRotating}
              className="w-full py-3 rounded-lg text-sm font-medium text-white bg-orange-500 hover:bg-orange-600 disabled:opacity-50 transition-colors flex items-center justify-center gap-2"
            >
              {isRotating ? (
                <>
                  <span className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                  {t('identity.rotating')}
                </>
              ) : (
                <>
                  <span>🔄</span>
                  {t('identity.rotate')}
                </>
              )}
            </button>
            <p className="mt-2 text-xs text-gray-500 dark:text-gray-400 text-center">
              {t('identity.rotateHint')}
            </p>
          </div>
        </div>
      ) : (
        <div className="text-center py-8 text-gray-500 dark:text-gray-400">
          <p className="text-sm">{t('identity.notFound')}</p>
          <button
            onClick={loadIdentity}
            className="mt-2 text-sm text-blue-500 hover:underline"
          >
            {t('identity.retry')}
          </button>
        </div>
      )}
    </div>
  );
}
