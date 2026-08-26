import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { ConfigPanel } from '../../components/ConfigPanel';
import { bleReadConfig, bleWriteWifi } from '../../hooks/useBle';

vi.mock('../../hooks/useBle', () => ({
  bleReadConfig: vi.fn(),
  bleWriteWifi: vi.fn(),
  bleWriteLlm: vi.fn(),
  bleWriteHostname: vi.fn(),
}));

vi.mock('../../utils/storage', () => ({
  ConfigStorage: {
    getConfig: vi.fn(() => null),
    saveConfig: vi.fn(),
  },
}));

const mockRead = vi.mocked(bleReadConfig);
const mockWriteWifi = vi.mocked(bleWriteWifi);

async function renderConnected(config: Record<string, unknown> = {}) {
  mockRead.mockResolvedValue({
    wifi_ssid: null,
    wifi_password: null,
    llm_model: null,
    llm_api_key: null,
    hostname: null,
    ...config,
  } as never);
  render(<ConfigPanel isConnected deviceId="dev-1" />);
  await act(async () => {});
}

describe('ConfigPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
  });

  it('shows a not-connected hint when disconnected', () => {
    render(<ConfigPanel isConnected={false} />);
    expect(screen.getByText('config.notConnected')).toBeInTheDocument();
  });

  it('loads configuration from the device when connected', async () => {
    await renderConnected();
    expect(mockRead).toHaveBeenCalledTimes(1);
  });

  it('validates that SSID is required before saving WiFi', async () => {
    await renderConnected();
    fireEvent.click(screen.getByRole('button', { name: 'config.wifi.save' }));
    expect(screen.getByText(/config\.wifi\.ssidRequired/)).toBeInTheDocument();
    expect(mockWriteWifi).not.toHaveBeenCalled();
  });

  it('saves WiFi with the entered ssid and password', async () => {
    mockWriteWifi.mockResolvedValue({ success: true, message: 'ok' } as never);
    await renderConnected();
    fireEvent.change(screen.getByPlaceholderText('config.wifi.ssidPlaceholder'), {
      target: { value: 'HomeWiFi' },
    });
    fireEvent.change(screen.getByPlaceholderText('config.wifi.passwordPlaceholder'), {
      target: { value: 'secret' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'config.wifi.save' }));
    expect(mockWriteWifi).toHaveBeenCalledWith('HomeWiFi', 'secret');
  });

  it('validates that the LLM model is required', async () => {
    await renderConnected();
    fireEvent.click(screen.getByRole('button', { name: 'config.llm.save' }));
    expect(screen.getByText(/config\.llm\.modelRequired/)).toBeInTheDocument();
  });
});
