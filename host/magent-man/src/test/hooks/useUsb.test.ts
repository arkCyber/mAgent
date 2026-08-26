import { describe, it, expect, vi, beforeEach } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { usbDeviceInfo } from '../../hooks/useUsb';

const mockInvoke = vi.mocked(invoke);

describe('usbDeviceInfo', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it('parses a full device-info snapshot from AT replies', async () => {
    mockInvoke.mockImplementation((_cmd: string, args?: any) => {
      switch (args?.cmd) {
        case 'AT+CWSTATE?':
          return Promise.resolve({ success: true, response: '+CWSTATE:5,192.168.1.10\r\nOK' });
        case 'AT+HEAP':
          return Promise.resolve({ success: true, response: '+HEAP:204800\r\nOK' });
        case 'AT+LLMCFG?':
          return Promise.resolve({ success: true, response: '+LLMCFG:deepseek-chat\r\nOK' });
        case 'AT+GMR':
          return Promise.resolve({ success: true, response: '+GMR:0.2.0\r\nOK' });
        case 'AT+UPTIME?':
          return Promise.resolve({ success: true, response: '+UPTIME:3600000\r\nOK' });
        default:
          return Promise.resolve({ success: true, response: 'OK' });
      }
    });

    const info = await usbDeviceInfo();
    expect(info.wifi).toBe('已连接');
    expect(info.ip).toBe('192.168.1.10');
    expect(info.heap).toBe('200 KB');
    expect(info.llm).toBe('deepseek-chat');
    expect(info.version).toBe('0.2.0');
    expect(info.uptime).toBe('3600s');
  });

  it('maps a WiFi state of 0 to "not connected" with no IP', async () => {
    mockInvoke.mockImplementation((_cmd: string, args?: any) => {
      if (args?.cmd === 'AT+CWSTATE?') {
        return Promise.resolve({ success: true, response: '+CWSTATE:0\r\nOK' });
      }
      return Promise.resolve({ success: true, response: 'ERROR' });
    });

    const info = await usbDeviceInfo();
    expect(info.wifi).toBe('未连接');
    expect(info.ip).toBeUndefined();
  });

  it('leaves fields undefined when all AT replies are errors', async () => {
    mockInvoke.mockResolvedValue({ success: true, response: 'ERROR' });
    const info = await usbDeviceInfo();
    expect(info).toEqual({});
  });
});
