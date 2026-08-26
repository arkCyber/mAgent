import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { ThemeProvider } from '../../contexts/ThemeContext';
import { ConfirmQuit } from '../../components/ConfirmQuit';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => ({ destroy: vi.fn() })),
}));

const mockListen = vi.mocked(listen);
const mockGetWindow = vi.mocked(getCurrentWindow);

describe('ConfirmQuit', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('is hidden by default', async () => {
    mockListen.mockResolvedValue(() => {});
    render(
      <ThemeProvider>
        <ConfirmQuit />
      </ThemeProvider>
    );
    await act(async () => {});
    expect(screen.queryByText('quit.title')).not.toBeInTheDocument();
  });

  it('shows the confirmation when the close-requested event fires', async () => {
    let trigger: (() => void) | undefined;
    mockListen.mockImplementation((_event: string, cb: (..._args: any[]) => void) => {
      trigger = cb;
      return Promise.resolve(() => {});
    });

    render(
      <ThemeProvider>
        <ConfirmQuit />
      </ThemeProvider>
    );
    await act(async () => {});

    await act(async () => {
      trigger?.();
    });
    expect(screen.getByText('quit.title')).toBeInTheDocument();
  });

  it('destroys the window on confirm', async () => {
    let trigger: (() => void) | undefined;
    mockListen.mockImplementation((_event: string, cb: (..._args: any[]) => void) => {
      trigger = cb;
      return Promise.resolve(() => {});
    });
    const destroy = vi.fn();
    mockGetWindow.mockReturnValue({ destroy } as never);

    render(
      <ThemeProvider>
        <ConfirmQuit />
      </ThemeProvider>
    );
    await act(async () => {});
    await act(async () => {
      trigger?.();
    });

    fireEvent.click(screen.getByRole('button', { name: 'quit.confirm' }));
    expect(destroy).toHaveBeenCalledTimes(1);
    expect(screen.queryByText('quit.title')).not.toBeInTheDocument();
  });

  it('does not destroy the window when cancelled', async () => {
    let trigger: (() => void) | undefined;
    mockListen.mockImplementation((_event: string, cb: (..._args: any[]) => void) => {
      trigger = cb;
      return Promise.resolve(() => {});
    });
    const destroy = vi.fn();
    mockGetWindow.mockReturnValue({ destroy } as never);

    render(
      <ThemeProvider>
        <ConfirmQuit />
      </ThemeProvider>
    );
    await act(async () => {});
    await act(async () => {
      trigger?.();
    });

    fireEvent.click(screen.getByRole('button', { name: 'quit.cancel' }));
    expect(destroy).not.toHaveBeenCalled();
    expect(screen.queryByText('quit.title')).not.toBeInTheDocument();
  });

  it('closes (cancels) on Escape without destroying', async () => {
    let trigger: (() => void) | undefined;
    mockListen.mockImplementation((_event: string, cb: (..._args: any[]) => void) => {
      trigger = cb;
      return Promise.resolve(() => {});
    });
    const destroy = vi.fn();
    mockGetWindow.mockReturnValue({ destroy } as never);

    render(
      <ThemeProvider>
        <ConfirmQuit />
      </ThemeProvider>
    );
    await act(async () => {});
    await act(async () => {
      trigger?.();
    });

    fireEvent.keyDown(window, { key: 'Escape' });
    expect(destroy).not.toHaveBeenCalled();
    expect(screen.queryByText('quit.title')).not.toBeInTheDocument();
  });
});
