import { useEffect, useState } from 'react';
import { getName, getVersion } from '@tauri-apps/api/app';

/**
 * Single source of truth for the application identity.
 *
 * At runtime we prefer the version reported by the Tauri bundle (which is
 * derived from `src-tauri/tauri.conf.json` / `Cargo.toml`). When running in a
 * plain browser (e.g. `vite` without Tauri) we fall back to the compiled-in
 * constant. Keep `APP_VERSION` in sync with `tauri.conf.json` and `Cargo.toml`.
 */
export const APP_NAME = 'mAgent-Man';
export const APP_VERSION = '0.2.0';

export interface AppInfo {
  name: string;
  version: string;
}

export function useAppInfo(): AppInfo {
  const [info, setInfo] = useState<AppInfo>({ name: APP_NAME, version: APP_VERSION });

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const [name, version] = await Promise.all([getName(), getVersion()]);
        if (alive) setInfo({ name: name || APP_NAME, version: version || APP_VERSION });
      } catch {
        // Not running inside Tauri — keep the compiled-in fallback.
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  return info;
}
