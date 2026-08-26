import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { Footer } from '../../components/Footer';

// useAppInfo talks to the Tauri app plugin — mock it out.
vi.mock('@tauri-apps/api/app', () => ({
  getName: vi.fn().mockResolvedValue('mAgent-Man'),
  getVersion: vi.fn().mockResolvedValue('0.2.0'),
}));

interface FooterOverrides {
  connectionState?: 'connected' | 'connecting' | 'error' | 'disconnected';
  connectedDeviceName?: string | null;
  deviceCount?: number;
  lastScanTime?: Date | null;
  onScan?: () => void;
  scanning?: boolean;
}

async function renderFooter(overrides: FooterOverrides = {}) {
  const utils = render(
    <Footer
      connectionState={overrides.connectionState ?? 'disconnected'}
      connectedDeviceName={overrides.connectedDeviceName ?? null}
      deviceCount={overrides.deviceCount ?? 0}
      lastScanTime={overrides.lastScanTime ?? null}
      onScan={overrides.onScan}
      scanning={overrides.scanning ?? false}
    />
  );
  await act(async () => {});
  return utils;
}

describe('Footer (status bar)', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('shows the disconnected status, device count and app version', async () => {
    await renderFooter({ deviceCount: 3 });
    expect(screen.getByText(/status\.disconnected/)).toBeInTheDocument();
    expect(screen.getByText(/3 footer\.devicesFound/)).toBeInTheDocument();
    expect(screen.getByText(/v0\.2\.0/)).toBeInTheDocument();
  });

  it('shows the connected device name when connected', async () => {
    await renderFooter({
      connectionState: 'connected',
      connectedDeviceName: 'mAgent-001',
    });
    expect(screen.getByText(/mAgent-001/)).toBeInTheDocument();
  });

  it('shows "never scanned" when there is no last scan', async () => {
    await renderFooter();
    expect(screen.getByText(/footer\.neverScanned/)).toBeInTheDocument();
  });

  it('shows a relative last-scan time when a scan has happened', async () => {
    const lastScanTime = new Date(Date.now() - 1000 * 10); // 10s ago
    await renderFooter({ lastScanTime });
    expect(screen.getByText(/10s footer\.ago/)).toBeInTheDocument();
  });

  describe('scan action', () => {
    it('calls onScan when the Scan button is clicked', async () => {
      const onScan = vi.fn();
      await renderFooter({ onScan });
      fireEvent.click(screen.getByRole('button', { name: /devices\.scan/ }));
      expect(onScan).toHaveBeenCalledTimes(1);
    });

    it('shows the scanning state when scanning is in progress', async () => {
      await renderFooter({ onScan: vi.fn(), scanning: true });
      expect(screen.getByText('devices.scanning')).toBeInTheDocument();
    });

    it('does not render a Scan button without onScan', async () => {
      await renderFooter();
      expect(screen.queryByRole('button', { name: /devices\.scan/ })).not.toBeInTheDocument();
    });
  });
});
