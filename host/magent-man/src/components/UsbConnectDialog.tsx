import { useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useTheme } from '../contexts/ThemeContext';

export type UsbPhase = 'scanning' | 'connecting' | 'connected' | 'error' | 'none';

interface UsbConnectDialogProps {
  open: boolean;
  phase: UsbPhase;
  /** Connected/connecting port path (when relevant). */
  path?: string | null;
  /** Error message (phase === 'error'). */
  error?: string | null;
  onClose: () => void;
  onRetry?: () => void;
}

const PHASE_COLORS: Record<UsbPhase, { bg: string; fg: string }> = {
  scanning: { bg: 'var(--color-warning-light)', fg: 'var(--color-warning)' },
  connecting: { bg: 'var(--color-warning-light)', fg: 'var(--color-warning)' },
  connected: { bg: 'var(--color-success-light)', fg: 'var(--color-success)' },
  error: { bg: 'var(--color-error-light)', fg: 'var(--color-error)' },
  none: { bg: 'var(--color-surface-hover)', fg: 'var(--color-text-muted)' },
};

/**
 * Modal shown while the app auto-scans USB ports and connects, ending in a
 * "connection successful" (or failed / no device) window for the user.
 */
export function UsbConnectDialog({
  open,
  phase,
  path,
  error,
  onClose,
  onRetry,
}: UsbConnectDialogProps) {
  const { t } = useTranslation();
  const { theme } = useTheme();
  const busy = phase === 'scanning' || phase === 'connecting';

  // Close on Escape (except while a busy phase is in progress).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !busy) onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, busy, onClose]);

  if (!open) return null;

  const isCoffee = theme === 'coffee';
  const colors = PHASE_COLORS[phase];

  const title =
    phase === 'connected'
      ? t('usb.dialogConnected')
      : phase === 'connecting'
        ? t('usb.dialogConnecting')
        : phase === 'scanning'
          ? t('usb.dialogScanning')
          : phase === 'error'
            ? t('usb.dialogFailed')
            : t('usb.dialogNoDevice');

  const description =
    phase === 'connected'
      ? t('usb.dialogConnectedHint', { path: path ?? 'USB' })
      : phase === 'connecting'
        ? t('usb.dialogConnectingHint', { path: path ?? 'USB' })
        : phase === 'scanning'
          ? t('usb.dialogScanningHint')
          : phase === 'error'
            ? error ?? t('usb.failed')
            : t('usb.dialogNoDeviceHint');

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center p-4 animate-fade-in"
      style={{ backgroundColor: 'rgba(0,0,0,0.45)', backdropFilter: 'blur(2px)' }}
    >
      <div
        className="relative w-full max-w-sm rounded-2xl shadow-2xl border p-6 text-center"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        style={{
          backgroundColor: 'var(--color-surface)',
          borderColor: 'var(--color-border)',
        }}
      >
        {/* Close (hidden while a busy phase is in progress) */}
        {!busy && (
          <button
            onClick={onClose}
            aria-label={t('common.close')}
            title={t('common.close')}
            className="absolute top-3 right-3 w-8 h-8 flex items-center justify-center rounded-lg transition-colors hover:scale-105"
            style={{ color: 'var(--color-text-muted)' }}
          >
            ✕
          </button>
        )}

        {/* Icon */}
        <div
          className="w-16 h-16 mx-auto flex items-center justify-center rounded-full text-3xl"
          style={{ backgroundColor: colors.bg, color: colors.fg }}
        >
          {busy ? (
            <span
              className="w-7 h-7 border-4 rounded-full animate-spin"
              style={{ borderColor: 'var(--color-warning-light)', borderTopColor: 'var(--color-warning)' }}
            />
          ) : phase === 'connected' ? (
            '✓'
          ) : phase === 'error' ? (
            '✕'
          ) : (
            '📡'
          )}
        </div>

        <h2
          className="mt-4 text-lg font-bold"
          style={{ color: 'var(--color-text)' }}
        >
          {title}
        </h2>
        <p
          className="mt-2 text-sm leading-relaxed break-words"
          style={{ color: 'var(--color-text-secondary)' }}
        >
          {description}
        </p>

        {/* Actions */}
        <div className="mt-6 flex items-center justify-center gap-3">
          {phase === 'connected' && (
            <button
              onClick={onClose}
              className="px-6 py-2.5 rounded-xl text-sm font-semibold text-white shadow-lg transition-all duration-200 hover:scale-105 active:scale-95"
              style={{
                background: isCoffee
                  ? 'linear-gradient(135deg,#d4a574,#c8956a)'
                  : 'var(--color-primary)',
              }}
            >
              {t('usb.start')}
            </button>
          )}

          {(phase === 'error' || phase === 'none') && onRetry && (
            <button
              onClick={onRetry}
              className="px-5 py-2.5 rounded-xl text-sm font-semibold text-white shadow-lg transition-all duration-200 hover:scale-105 active:scale-95"
              style={{ background: 'var(--color-primary)' }}
            >
              {t('usb.retry')}
            </button>
          )}

          {(phase === 'error' || phase === 'none') && (
            <button
              onClick={onClose}
              className="px-5 py-2.5 rounded-xl text-sm font-medium transition-all duration-200 hover:scale-105"
              style={{
                backgroundColor: 'var(--color-surface-hover)',
                color: 'var(--color-text-secondary)',
              }}
            >
              {t('usb.continue')}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
