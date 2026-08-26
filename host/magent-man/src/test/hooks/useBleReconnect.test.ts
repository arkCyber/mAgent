import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { useBleReconnect } from '../../hooks/useBleReconnect';
import { bleConnect, bleDisconnect } from '../../hooks/useBle';

vi.mock('../../hooks/useBle', () => ({
  bleConnect: vi.fn(),
  bleDisconnect: vi.fn(),
  bleStatus: vi.fn(),
}));

const mockConnect = vi.mocked(bleConnect);
const mockDisconnect = vi.mocked(bleDisconnect);

const DEVICE = { id: 'dev-1', name: 'mAgent-001', rssi: -50 };

function renderReconnect(options: { maxRetries?: number } = {}) {
  const onConnected = vi.fn();
  const onDisconnected = vi.fn();
  const onError = vi.fn();
  const { result } = renderHook(() =>
    useBleReconnect({
      maxRetries: options.maxRetries ?? 2,
      baseDelay: 5,
      maxDelay: 50,
      onConnected,
      onDisconnected,
      onError,
    })
  );
  return { result, onConnected, onDisconnected, onError };
}

describe('useBleReconnect', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('connects successfully on the first attempt', async () => {
    mockConnect.mockResolvedValue({ success: true, message: 'ok' });
    const { result, onConnected } = renderReconnect();

    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.connect(DEVICE);
    });
    expect(ok).toBe(true);
    expect(onConnected).toHaveBeenCalledTimes(1);
  });

  it('retries and eventually connects after a transient failure', async () => {
    mockConnect
      .mockResolvedValueOnce({ success: false, message: 'busy' })
      .mockResolvedValue({ success: true, message: 'ok' });
    const { result, onConnected } = renderReconnect();

    let ok: boolean | undefined;
    await act(async () => {
      ok = await result.current.connect(DEVICE);
    });
    expect(ok).toBe(false); // first attempt failed
    await waitFor(() => expect(onConnected).toHaveBeenCalledTimes(1), { timeout: 2000 });
  });

  it('gives up and reports an error after exhausting retries', async () => {
    mockConnect.mockResolvedValue({ success: false, message: 'busy' });
    const { result, onError } = renderReconnect({ maxRetries: 2 });

    await act(async () => {
      await result.current.connect(DEVICE);
    });
    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1), { timeout: 2000 });
  });

  it('disconnect closes the connection and clears state', async () => {
    mockConnect.mockResolvedValue({ success: true, message: 'ok' });
    mockDisconnect.mockResolvedValue({ success: true, message: 'closed' });
    const { result, onConnected, onDisconnected } = renderReconnect();

    await act(async () => {
      await result.current.connect(DEVICE);
      await result.current.disconnect();
    });
    expect(onConnected).toHaveBeenCalledTimes(1);
    expect(onDisconnected).toHaveBeenCalledTimes(1);
    expect(mockDisconnect).toHaveBeenCalledTimes(1);
  });
});
