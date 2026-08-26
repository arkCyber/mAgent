import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useTranslation } from 'react-i18next';
import { useTheme } from '../contexts/ThemeContext';

const CLOSE_REQUEST_EVENT = 'magent://close-requested';

/**
 * Listens for the backend's close request (the window close was intercepted in
 * Rust) and shows a confirmation dialog. Only on confirm does the app actually
 * quit (via `getCurrentWindow().destroy()`).
 */
export function ConfirmQuit() {
  const { t } = useTranslation();
  const { theme } = useTheme();
  const [open, setOpen] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let alive = true;
    (async () => {
      try {
        unlisten = await listen(CLOSE_REQUEST_EVENT, () => {
          if (alive) setOpen(true);
        });
      } catch {
        // Not running inside Tauri (e.g. plain vite in a browser) — no-op.
      }
    })();
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  const confirmQuit = () => {
    setOpen(false);
    try {
      getCurrentWindow().destroy();
    } catch {
      // Outside Tauri there is nothing to destroy.
    }
  };

  // Close (cancel) on Escape.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open]);

  if (!open) return null;

  const isCoffee = theme === 'coffee';

  return (
    <div
      className="fixed inset-0 z-[120] flex items-center justify-center p-4 animate-fade-in"
      style={{ backgroundColor: 'rgba(0,0,0,0.45)', backdropFilter: 'blur(2px)' }}
    >
      <div
        className="w-full max-w-sm rounded-2xl shadow-2xl border p-6 text-center"
        role="dialog"
        aria-modal="true"
        aria-label={t('quit.title')}
        style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}
      >
        <div className="text-4xl">🚪</div>
        <h2 className="mt-3 text-lg font-bold" style={{ color: 'var(--color-text)' }}>
          {t('quit.title')}
        </h2>
        <p className="mt-2 text-sm text-center" style={{ color: 'var(--color-text-secondary)' }}>
          {t('quit.message')}
        </p>

        <div className="mt-6 flex items-center justify-center gap-3">
          <button
            onClick={confirmQuit}
            className="px-6 py-2.5 rounded-xl text-sm font-semibold text-white shadow-lg transition-all duration-200 hover:scale-105 active:scale-95"
            style={{
              background: isCoffee
                ? 'linear-gradient(135deg,#e06666,#c84a4a)'
                : 'var(--color-error)',
            }}
          >
            {t('quit.confirm')}
          </button>
          <button
            onClick={() => setOpen(false)}
            className="px-6 py-2.5 rounded-xl text-sm font-medium transition-all duration-200 hover:scale-105"
            style={{
              backgroundColor: 'var(--color-surface-hover)',
              color: 'var(--color-text-secondary)',
            }}
          >
            {t('quit.cancel')}
          </button>
        </div>
      </div>
    </div>
  );
}
