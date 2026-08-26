import '@testing-library/jest-dom';
import { vi } from 'vitest';

// Some Node versions / jsdom bundles do not expose `localStorage` (Node 22+
// ships an experimental global that is `undefined` without --localstorage-file).
// The app persists theme + language to localStorage, so polyfill it in-memory.
if (typeof localStorage === 'undefined') {
  const store = new Map<string, string>();
  const localStorageMock: Storage = {
    getItem: (key: string) => (store.has(key) ? store.get(key) as string : null),
    setItem: (key: string, value: string) => { store.set(key, String(value)); },
    removeItem: (key: string) => { store.delete(key); },
    clear: () => { store.clear(); },
    key: (index: number) => Array.from(store.keys())[index] ?? null,
    get length() { return store.size; },
  } as Storage;
  Object.defineProperty(globalThis, 'localStorage', {
    value: localStorageMock,
    writable: true,
  });
  if (typeof window !== 'undefined') {
    Object.defineProperty(window, 'localStorage', {
      value: localStorageMock,
      writable: true,
    });
  }
}

// Mock Tauri API
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

// Mock i18next with proper interpolation. `t`/`i18n` are defined once so their
// references are stable across renders (mirroring react-i18next) — otherwise a
// component that puts `t` in a useCallback/useEffect dependency would re-run in
// an infinite loop.
vi.mock('react-i18next', () => {
  const changeLanguage = vi.fn();
  const i18n = { language: 'en', changeLanguage };
  const t = (key: string, options?: { [key: string]: string | number }) => {
    // Simple interpolation: replace {name} with options.name
    let result = key;
    if (options) {
      Object.entries(options).forEach(([k, v]) => {
        result = result.replace(new RegExp(`\\{${k}\\}`, 'g'), String(v));
      });
    }
    return result;
  };
  return {
    useTranslation: () => ({ t, i18n }),
    initReactI18next: {
      type: '3rdParty',
      init: vi.fn(),
    },
  };
});
