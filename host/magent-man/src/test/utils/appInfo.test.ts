import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { useAppInfo, APP_NAME, APP_VERSION } from '../../hooks/useAppInfo';

vi.mock('@tauri-apps/api/app', () => ({
  getName: vi.fn(),
  getVersion: vi.fn(),
}));

import { getName, getVersion } from '@tauri-apps/api/app';

const mockedGetName = vi.mocked(getName);
const mockedGetVersion = vi.mocked(getVersion);

describe('useAppInfo', () => {
  beforeEach(() => {
    mockedGetName.mockReset();
    mockedGetVersion.mockReset();
  });

  it('exports a single source of truth for name and version', () => {
    expect(APP_NAME).toBe('mAgent-Man');
    // Keep in sync with src-tauri/Cargo.toml and src-tauri/tauri.conf.json
    expect(APP_VERSION).toBe('0.2.0');
  });

  it('falls back to compiled-in values outside Tauri', async () => {
    mockedGetName.mockRejectedValue(new Error('no tauri'));
    mockedGetVersion.mockRejectedValue(new Error('no tauri'));

    const { result } = renderHook(() => useAppInfo());
    await waitFor(() => {
      expect(result.current).toEqual({ name: APP_NAME, version: APP_VERSION });
    });
  });

  it('uses the runtime values reported by Tauri', async () => {
    mockedGetName.mockResolvedValue('mAgent-Man');
    mockedGetVersion.mockResolvedValue('9.9.9');

    const { result } = renderHook(() => useAppInfo());
    await waitFor(() => {
      expect(result.current).toEqual({ name: 'mAgent-Man', version: '9.9.9' });
    });
  });
});
