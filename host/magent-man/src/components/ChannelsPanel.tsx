import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { bleGetChannels } from '../hooks/useBle';

interface ChatPanelProps {
  isConnected: boolean;
}

interface Channel {
  id: string;
  type: 'local' | 'mqtt' | 'webhook' | 'web3' | 'manual';
  name: string;
  status: 'active' | 'inactive' | 'error';
  description: string;
  icon: string;
  messages: number;
  lastActivity: Date | null;
}

export function ChannelsPanel({ isConnected }: ChatPanelProps) {
  const { t } = useTranslation();
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(false);
  const [selectedChannel, setSelectedChannel] = useState<Channel | null>(null);
  const [filter, setFilter] = useState<'all' | 'active' | 'inactive'>('all');

  // Default channels based on architecture analysis
  const defaultChannels: Channel[] = [
    {
      id: 'local-ble',
      type: 'local',
      name: 'Local BLE',
      status: isConnected ? 'active' : 'inactive',
      description: t('channels.local.description'),
      icon: '📡',
      messages: 0,
      lastActivity: null,
    },
    {
      id: 'local-uart',
      type: 'local',
      name: 'Local UART',
      status: 'active',
      description: t('channels.uart.description'),
      icon: '🔌',
      messages: 0,
      lastActivity: null,
    },
    {
      id: 'manual',
      type: 'manual',
      name: 'Manual Input',
      status: 'active',
      description: t('channels.manual.description'),
      icon: '⌨️',
      messages: 0,
      lastActivity: null,
    },
    {
      id: 'mqtt',
      type: 'mqtt',
      name: 'MQTT',
      status: 'inactive',
      description: t('channels.mqtt.description'),
      icon: '📨',
      messages: 0,
      lastActivity: null,
    },
    {
      id: 'webhook',
      type: 'webhook',
      name: 'Webhook',
      status: 'inactive',
      description: t('channels.webhook.description'),
      icon: '🌐',
      messages: 0,
      lastActivity: null,
    },
    {
      id: 'web3',
      type: 'web3',
      name: 'Web3',
      status: 'inactive',
      description: t('channels.web3.description'),
      icon: '⛓️',
      messages: 0,
      lastActivity: null,
    },
  ];

  const loadChannels = useCallback(async () => {
    if (!isConnected) {
      setChannels(defaultChannels);
      return;
    }

    setLoading(true);
    try {
      const deviceChannels = await bleGetChannels();

      // Merge with default channels
      const mergedChannels = defaultChannels.map(ch => {
        const deviceCh = deviceChannels.find(dc => dc.id === ch.id);
        if (deviceCh) {
          return {
            ...ch,
            status: deviceCh.status as 'active' | 'inactive' | 'error',
            messages: deviceCh.messages || 0,
            lastActivity: deviceCh.lastActivity ? new Date(deviceCh.lastActivity) : null,
          };
        }
        return ch;
      });

      setChannels(mergedChannels);
    } catch (e) {
      console.error('Failed to load channels:', e);
      setChannels(defaultChannels);
    } finally {
      setLoading(false);
    }
  }, [isConnected, t]);

  useEffect(() => {
    loadChannels();
    const interval = setInterval(loadChannels, 30000);
    return () => clearInterval(interval);
  }, [loadChannels]);

  const getStatusColor = (status: string) => {
    switch (status) {
      case 'active':
        return 'bg-green-500';
      case 'inactive':
        return 'bg-gray-400';
      case 'error':
        return 'bg-red-500';
      default:
        return 'bg-gray-400';
    }
  };

  const getStatusText = (status: string) => {
    switch (status) {
      case 'active':
        return t('channels.status.active');
      case 'inactive':
        return t('channels.status.inactive');
      case 'error':
        return t('channels.status.error');
      default:
        return status;
    }
  };

  const formatLastActivity = (date: Date | null) => {
    if (!date) return t('channels.never');
    const diff = Math.floor((Date.now() - date.getTime()) / 1000);
    if (diff < 60) return `${diff}s ${t('channels.ago')}`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m ${t('channels.ago')}`;
    return date.toLocaleTimeString();
  };

  const filteredChannels = channels.filter(ch => {
    if (filter === 'all') return true;
    if (filter === 'active') return ch.status === 'active';
    if (filter === 'inactive') return ch.status === 'inactive';
    return true;
  });

  const activeCount = channels.filter(ch => ch.status === 'active').length;
  const totalMessages = channels.reduce((sum, ch) => sum + ch.messages, 0);

  if (!isConnected) {
    return (
      <div>
        <div className="flex items-center justify-between mb-5">
          <h2 className="text-xl font-semibold">{t('channels.title')}</h2>
        </div>
        <div className="flex flex-col items-center justify-center py-16 text-center bg-white dark:bg-gray-800 rounded-xl">
          <span className="text-5xl opacity-30">🔗</span>
          <h3 className="mt-4 text-lg font-medium">{t('channels.notConnected')}</h3>
          <p className="mt-2 text-gray-500 dark:text-gray-400">{t('channels.notConnectedHint')}</p>
        </div>
      </div>
    );
  }

  return (
    <div>
      {/* Header */}
      <div className="flex items-center justify-between mb-5">
        <h2 className="text-xl font-semibold">{t('channels.title')}</h2>
        <div className="flex items-center gap-2">
          <button
            onClick={loadChannels}
            disabled={loading}
            className="w-9 h-9 flex items-center justify-center bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
          >
            <span className={loading ? 'animate-spin' : ''}>↻</span>
          </button>
        </div>
      </div>

      {/* Summary Cards */}
      <div className="grid grid-cols-3 gap-4 mb-6">
        <div className="bg-white dark:bg-gray-800 rounded-xl p-4 shadow-sm">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center">
              <span className="text-lg">✓</span>
            </div>
            <div>
              <p className="text-2xl font-bold">{activeCount}</p>
              <p className="text-xs text-gray-500">{t('channels.activeChannels')}</p>
            </div>
          </div>
        </div>
        <div className="bg-white dark:bg-gray-800 rounded-xl p-4 shadow-sm">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-full bg-blue-100 dark:bg-blue-900/30 flex items-center justify-center">
              <span className="text-lg">💬</span>
            </div>
            <div>
              <p className="text-2xl font-bold">{totalMessages}</p>
              <p className="text-xs text-gray-500">{t('channels.totalMessages')}</p>
            </div>
          </div>
        </div>
        <div className="bg-white dark:bg-gray-800 rounded-xl p-4 shadow-sm">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-full bg-purple-100 dark:bg-purple-900/30 flex items-center justify-center">
              <span className="text-lg">📡</span>
            </div>
            <div>
              <p className="text-2xl font-bold">{channels.length}</p>
              <p className="text-xs text-gray-500">{t('channels.totalChannels')}</p>
            </div>
          </div>
        </div>
      </div>

      {/* Filter */}
      <div className="flex gap-2 mb-4">
        <button
          onClick={() => setFilter('all')}
          className={`px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
            filter === 'all'
              ? 'bg-blue-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600'
          }`}
        >
          {t('channels.filter.all')}
        </button>
        <button
          onClick={() => setFilter('active')}
          className={`px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
            filter === 'active'
              ? 'bg-green-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600'
          }`}
        >
          {t('channels.filter.active')}
        </button>
        <button
          onClick={() => setFilter('inactive')}
          className={`px-4 py-2 rounded-lg text-sm font-medium transition-colors ${
            filter === 'inactive'
              ? 'bg-gray-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600'
          }`}
        >
          {t('channels.filter.inactive')}
        </button>
      </div>

      {/* Channel List */}
      <div className="space-y-3">
        {filteredChannels.map((channel) => (
          <div
            key={channel.id}
            onClick={() => setSelectedChannel(channel)}
            className={`bg-white dark:bg-gray-800 rounded-xl p-4 shadow-sm cursor-pointer transition-all hover:shadow-md ${
              selectedChannel?.id === channel.id ? 'ring-2 ring-blue-500' : ''
            }`}
          >
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-4">
                <div className="w-12 h-12 rounded-xl bg-gray-100 dark:bg-gray-700 flex items-center justify-center text-2xl">
                  {channel.icon}
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <h3 className="font-medium">{channel.name}</h3>
                    <span className={`w-2 h-2 rounded-full ${getStatusColor(channel.status)}`} />
                    <span className="text-xs text-gray-500">{getStatusText(channel.status)}</span>
                  </div>
                  <p className="text-sm text-gray-500 dark:text-gray-400">{channel.description}</p>
                </div>
              </div>
              <div className="text-right">
                <p className="text-sm font-medium">{channel.messages} {t('channels.messages')}</p>
                <p className="text-xs text-gray-400">{formatLastActivity(channel.lastActivity)}</p>
              </div>
            </div>
          </div>
        ))}
      </div>

      {/* Channel Detail Modal */}
      {selectedChannel && (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
          <div className="bg-white dark:bg-gray-800 rounded-2xl p-6 w-full max-w-md mx-4">
            <div className="flex items-center justify-between mb-4">
              <div className="flex items-center gap-3">
                <div className="w-12 h-12 rounded-xl bg-gray-100 dark:bg-gray-700 flex items-center justify-center text-2xl">
                  {selectedChannel.icon}
                </div>
                <div>
                  <h3 className="text-lg font-semibold">{selectedChannel.name}</h3>
                  <span className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs text-white ${getStatusColor(selectedChannel.status)}`}>
                    {getStatusText(selectedChannel.status)}
                  </span>
                </div>
              </div>
              <button
                onClick={() => setSelectedChannel(null)}
                className="w-8 h-8 flex items-center justify-center hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"
              >
                ×
              </button>
            </div>

            <div className="space-y-4">
              <div>
                <label className="text-xs font-medium text-gray-500 uppercase">{t('channels.type')}</label>
                <p className="text-sm capitalize">{selectedChannel.type}</p>
              </div>
              <div>
                <label className="text-xs font-medium text-gray-500 uppercase">{t('channels.description')}</label>
                <p className="text-sm">{selectedChannel.description}</p>
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <label className="text-xs font-medium text-gray-500 uppercase">{t('channels.messages')}</label>
                  <p className="text-lg font-bold">{selectedChannel.messages}</p>
                </div>
                <div>
                  <label className="text-xs font-medium text-gray-500 uppercase">{t('channels.lastActivity')}</label>
                  <p className="text-sm">{formatLastActivity(selectedChannel.lastActivity)}</p>
                </div>
              </div>
            </div>

            <div className="mt-6 flex gap-2">
              <button
                onClick={() => setSelectedChannel(null)}
                className="flex-1 px-4 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg text-sm font-medium transition-colors"
              >
                {t('channels.close')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
