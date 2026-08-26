import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ChannelsPanel } from '../../components/ChannelsPanel';
import { bleGetChannels } from '../../hooks/useBle';

vi.mock('../../hooks/useBle', () => ({ bleGetChannels: vi.fn() }));
const mockChannels = vi.mocked(bleGetChannels);

describe('ChannelsPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('shows the not-connected state when disconnected', () => {
    render(<ChannelsPanel isConnected={false} />);
    expect(screen.getByText('channels.notConnected')).toBeInTheDocument();
    expect(mockChannels).not.toHaveBeenCalled();
  });

  it('renders the default channels when connected', async () => {
    mockChannels.mockResolvedValue([]);
    render(<ChannelsPanel isConnected />);
    expect(await screen.findByText('Local BLE')).toBeInTheDocument();
    expect(screen.getByText('Local UART')).toBeInTheDocument();
    expect(screen.getByText('MQTT')).toBeInTheDocument();
    expect(mockChannels).toHaveBeenCalled();
  });
});
