import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { useUsbAutoConnect } from '../../hooks/useUsbAutoConnect';
import { usbListPorts, usbOpen, usbGetStatus, usbClose } from '../../hooks/useUsb';

vi.mock('../../hooks/useUsb', () => ({
  usbListPorts: vi.fn(),
  usbOpen: vi.fn(),
  usbGetStatus: vi.fn(),
  usbClose: vi.fn(),
}));

const mockList = vi.mocked(usbListPorts);
const mockOpen = vi.mocked(usbOpen);
const mockStatus = vi.mocked(usbGetStatus);
const mockClose = vi.mocked(usbClose);

describe('useUsbAutoConnect', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('auto-scans and connects to the first USB port on mount', async () => {
    mockStatus.mockResolvedValue({ connected: false, path: null });
    mockList.mockResolvedValue(['/dev/cu.usbserial-10', '/dev/cu.usbserial-11']);
    mockOpen.mockResolvedValue({ success: true, path: '/dev/cu.usbserial-10' });

    const { result } = renderHook(() => useUsbAutoConnect());

    await waitFor(() => expect(result.current.connectedPath).toBe('/dev/cu.usbserial-10'));
    expect(mockList).toHaveBeenCalledTimes(1);
    expect(mockOpen).toHaveBeenCalledWith('/dev/cu.usbserial-10');
    expect(result.current.ports).toEqual(['/dev/cu.usbserial-10', '/dev/cu.usbserial-11']);
  });

  it('does not reopen when the backend is already connected', async () => {
    mockStatus.mockResolvedValue({ connected: true, path: '/dev/cu.usbserial-5' });

    const { result } = renderHook(() => useUsbAutoConnect());

    await waitFor(() => expect(result.current.connectedPath).toBe('/dev/cu.usbserial-5'));
    expect(mockList).not.toHaveBeenCalled();
    expect(mockOpen).not.toHaveBeenCalled();
  });

  it('does not flash the scanning state when already connected', async () => {
    mockStatus.mockResolvedValue({ connected: true, path: '/dev/cu.usbserial-5' });

    const { result } = renderHook(() => useUsbAutoConnect());
    await waitFor(() => expect(result.current.connectedPath).toBe('/dev/cu.usbserial-5'));

    // Because the backend is already connected, autoConnect reflects it without
    // ever entering the "scanning" state (no visible re-connect flash).
    expect(result.current.scanning).toBe(false);
    expect(result.current.connecting).toBe(false);
  });

  it('does not re-open the port when autoConnect is called again after success', async () => {
    mockStatus.mockResolvedValue({ connected: false, path: null });
    mockList.mockResolvedValue(['/dev/cu.usbserial-10']);
    mockOpen.mockResolvedValue({ success: true, path: '/dev/cu.usbserial-10' });

    const { result } = renderHook(() => useUsbAutoConnect());
    await waitFor(() => expect(result.current.connectedPath).toBe('/dev/cu.usbserial-10'));
    expect(mockOpen).toHaveBeenCalledTimes(1);

    // A later autoConnect (e.g. a stray retry) must NOT reopen the port.
    await act(async () => {
      await result.current.autoConnect();
      await result.current.autoConnect();
    });
    expect(mockOpen).toHaveBeenCalledTimes(1);
  });

  it('reconnects after a manual disconnect', async () => {
    mockStatus.mockResolvedValue({ connected: false, path: null });
    mockList.mockResolvedValue(['/dev/cu.usbserial-10']);
    mockOpen.mockResolvedValue({ success: true, path: '/dev/cu.usbserial-10' });
    mockClose.mockResolvedValue({ success: true });

    const { result } = renderHook(() => useUsbAutoConnect());
    await waitFor(() => expect(result.current.connectedPath).toBe('/dev/cu.usbserial-10'));

    await act(async () => {
      await result.current.disconnect();
    });
    expect(result.current.connectedPath).toBeNull();

    await act(async () => {
      await result.current.autoConnect();
    });
    expect(mockOpen).toHaveBeenCalledTimes(2);
    expect(result.current.connectedPath).toBe('/dev/cu.usbserial-10');
  });

  it('sets an error when no USB ports are found', async () => {
    mockStatus.mockResolvedValue({ connected: false, path: null });
    mockList.mockResolvedValue([]);

    const { result } = renderHook(() => useUsbAutoConnect());

    await waitFor(() => expect(result.current.error).toBeTruthy());
    expect(mockOpen).not.toHaveBeenCalled();
    expect(result.current.connectedPath).toBeNull();
  });

  it('reports an error when opening the port fails', async () => {
    mockStatus.mockResolvedValue({ connected: false, path: null });
    mockList.mockResolvedValue(['/dev/cu.usbserial-10']);
    mockOpen.mockRejectedValue(new Error('Permission denied'));

    const { result } = renderHook(() => useUsbAutoConnect());

    await waitFor(() => expect(result.current.error).toContain('Permission denied'));
    expect(result.current.connectedPath).toBeNull();
  });

  it('disconnect closes the port and clears the connection', async () => {
    mockStatus.mockResolvedValue({ connected: false, path: null });
    mockList.mockResolvedValue(['/dev/cu.usbserial-10']);
    mockOpen.mockResolvedValue({ success: true, path: '/dev/cu.usbserial-10' });
    mockClose.mockResolvedValue({ success: true });

    const { result } = renderHook(() => useUsbAutoConnect());
    await waitFor(() => expect(result.current.connectedPath).toBe('/dev/cu.usbserial-10'));

    await act(async () => {
      await result.current.disconnect();
    });
    expect(mockClose).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(result.current.connectedPath).toBeNull());
  });
});
