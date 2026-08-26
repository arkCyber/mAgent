import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { WifiScanner } from '../../components/WifiScanner';
import { ToastProvider } from '../../components/Toast';
import { bleExecCommand } from '../../hooks/useBle';

vi.mock('../../hooks/useBle', () => ({ bleExecCommand: vi.fn() }));
const mockExec = vi.mocked(bleExecCommand);

function renderScanner(isConnected: boolean, onSelectNetwork = vi.fn()) {
  return render(
    <ToastProvider>
      <WifiScanner isConnected={isConnected} onSelectNetwork={onSelectNetwork} />
    </ToastProvider>
  );
}

describe('WifiScanner', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('shows the not-connected hint and disables scan when disconnected', () => {
    renderScanner(false);
    expect(screen.getByText('wifiScan.notConnected')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /wifiScan\.scan/ })).toBeDisabled();
    expect(mockExec).not.toHaveBeenCalled();
  });

  it('scans, lists networks and reports the selected one', async () => {
    mockExec.mockResolvedValue({
      success: true,
      message: '+CWLAP:(3,"HomeWiFi",-45,6)\r\nOK',
    } as never);
    const onSelectNetwork = vi.fn();
    renderScanner(true, onSelectNetwork);

    fireEvent.click(screen.getByRole('button', { name: /wifiScan\.scan/ }));
    await act(async () => {});

    const network = screen.getByText('HomeWiFi');
    fireEvent.click(network);
    expect(onSelectNetwork).toHaveBeenCalledWith('HomeWiFi');
  });
});
