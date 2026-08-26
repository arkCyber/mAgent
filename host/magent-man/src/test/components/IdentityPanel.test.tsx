import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { IdentityPanel } from '../../components/IdentityPanel';
import { ToastProvider } from '../../components/Toast';
import { bleExecCommand } from '../../hooks/useBle';

vi.mock('../../hooks/useBle', () => ({ bleExecCommand: vi.fn() }));
const mockExec = vi.mocked(bleExecCommand);

function renderIdentity(isConnected: boolean) {
  return render(
    <ToastProvider>
      <IdentityPanel isConnected={isConnected} />
    </ToastProvider>
  );
}

describe('IdentityPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('shows the not-connected hint when disconnected', () => {
    renderIdentity(false);
    expect(screen.getByText('identity.notConnected')).toBeInTheDocument();
    expect(mockExec).not.toHaveBeenCalled();
  });

  it('loads and displays the derived address when connected', async () => {
    mockExec.mockResolvedValue({
      success: true,
      message: `+IDENT:${'a'.repeat(64)}`,
    } as never);
    renderIdentity(true);

    expect(await screen.findByText(new RegExp(`^0x${'a'.repeat(40)}$`))).toBeInTheDocument();
    expect(mockExec).toHaveBeenCalledWith('AT+IDENT?');
  });
});
