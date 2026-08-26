import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { ThemeProvider } from '../../contexts/ThemeContext';
import { UsbChatPanel } from '../../components/UsbChatPanel';
import { usbGetStatus, usbAgentChat } from '../../hooks/useUsb';

vi.mock('../../hooks/useUsb', () => ({
  usbGetStatus: vi.fn(),
  usbListPorts: vi.fn(),
  usbOpen: vi.fn(),
  usbClose: vi.fn(),
  usbSendAt: vi.fn(),
  usbAgentChat: vi.fn(),
}));

const mockGetStatus = vi.mocked(usbGetStatus);
const mockAgentChat = vi.mocked(usbAgentChat);

describe('UsbChatPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockGetStatus.mockResolvedValue({ connected: false, path: null });
  });

  it('reflects the app-level USB auto-connect with a welcome message', async () => {
    render(
      <ThemeProvider>
        <UsbChatPanel autoConnected autoPath="/dev/cu.usbserial-10" />
      </ThemeProvider>
    );
    await act(async () => {});
    expect(screen.getByText(/已通过 USB 连接到 \/dev\/cu\.usbserial-10/)).toBeInTheDocument();
    // The connected header also shows the port path.
    expect(screen.getAllByText(/\/dev\/cu\.usbserial-10/).length).toBeGreaterThan(0);
  });

  it('shows the not-connected state when no auto-connect happened', async () => {
    render(
      <ThemeProvider>
        <UsbChatPanel />
      </ThemeProvider>
    );
    await act(async () => {});
    expect(screen.queryByText(/已通过 USB 连接到/)).not.toBeInTheDocument();
    expect(screen.getByText(/通过 USB 串口连接 C61/)).toBeInTheDocument();
  });

  it('sends a message and renders the agent reply', async () => {
    mockAgentChat.mockResolvedValue({ success: true, response: '你好！' });
    render(
      <ThemeProvider>
        <UsbChatPanel autoConnected autoPath="/dev/cu.usbserial-10" />
      </ThemeProvider>
    );
    await act(async () => {});

    fireEvent.change(screen.getByPlaceholderText('输入消息，例如：内存还剩多少'), {
      target: { value: '你好吗' },
    });
    fireEvent.click(screen.getByRole('button', { name: '发送' }));

    // User message appears immediately; agent reply arrives after the async call.
    expect(screen.getByText('你好吗')).toBeInTheDocument();
    expect(await screen.findByText('你好！')).toBeInTheDocument();
    expect(mockAgentChat).toHaveBeenCalledWith('你好吗');
  });

  it('renders an error message when the agent call fails', async () => {
    mockAgentChat.mockRejectedValue(new Error('USB AT failed'));
    render(
      <ThemeProvider>
        <UsbChatPanel autoConnected autoPath="/dev/cu.usbserial-10" />
      </ThemeProvider>
    );
    await act(async () => {});

    fireEvent.change(screen.getByPlaceholderText('输入消息，例如：内存还剩多少'), {
      target: { value: '测试' },
    });
    fireEvent.click(screen.getByRole('button', { name: '发送' }));

    expect(await screen.findByText(/USB AT failed/)).toBeInTheDocument();
  });
});
