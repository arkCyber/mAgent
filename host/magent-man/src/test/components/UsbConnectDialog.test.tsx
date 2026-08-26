import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { ThemeProvider } from '../../contexts/ThemeContext';
import { UsbConnectDialog, type UsbPhase } from '../../components/UsbConnectDialog';

interface DialogProps {
  open?: boolean;
  phase?: UsbPhase;
  path?: string | null;
  error?: string | null;
  onClose?: () => void;
  onRetry?: () => void;
}

function renderDialog(props: DialogProps = {}) {
  const onClose = vi.fn();
  const onRetry = vi.fn();
  render(
    <ThemeProvider>
      <UsbConnectDialog
        open={props.open ?? true}
        phase={props.phase ?? 'scanning'}
        path={props.path ?? null}
        error={props.error ?? null}
        onClose={props.onClose ?? onClose}
        onRetry={props.onRetry ?? onRetry}
      />
    </ThemeProvider>
  );
  return { onClose, onRetry };
}

describe('UsbConnectDialog', () => {
  it('renders nothing when closed', () => {
    const { container } = render(
      <ThemeProvider>
        <UsbConnectDialog open={false} phase="scanning" onClose={() => {}} />
      </ThemeProvider>
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the scanning state', () => {
    renderDialog({ phase: 'scanning' });
    expect(screen.getByText('usb.dialogScanning')).toBeInTheDocument();
  });

  it('shows the connecting state', () => {
    renderDialog({ phase: 'connecting' });
    expect(screen.getByText('usb.dialogConnecting')).toBeInTheDocument();
  });

  it('shows the success window and closes via Start', () => {
    const { onClose } = renderDialog({ phase: 'connected', path: '/dev/cu.usbserial-10' });
    expect(screen.getByText('usb.dialogConnected')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'usb.start' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('shows the error state with Retry and Continue actions', () => {
    const { onClose, onRetry } = renderDialog({
      phase: 'error',
      error: 'Permission denied',
    });
    expect(screen.getByText('usb.dialogFailed')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'usb.retry' }));
    expect(onRetry).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: 'usb.continue' }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('shows the no-device state', () => {
    renderDialog({ phase: 'none' });
    expect(screen.getByText('usb.dialogNoDevice')).toBeInTheDocument();
  });

  it('closes on Escape for a non-busy phase', () => {
    const { onClose } = renderDialog({ phase: 'connected' });
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('does not close on Escape during a busy phase', () => {
    const { onClose } = renderDialog({ phase: 'scanning' });
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
  });
});
