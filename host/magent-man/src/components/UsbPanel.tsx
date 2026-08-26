import { useState, useCallback, useRef, useEffect } from 'react';
import { useTheme } from '../contexts/ThemeContext';
import {
  usbListPorts,
  usbOpen,
  usbClose,
  usbSendAt,
  usbAgentChat,
  usbDeviceInfo,
  type UsbDeviceInfo,
} from '../hooks/useUsb';

interface ChatMsg {
  role: 'user' | 'agent' | 'system';
  text: string;
  time?: string;
}

interface AtItem {
  cmd: string;
  resp: string;
}

/**
 * USB-serial panel: connect to the C61 over its UART0 console port (instead of
 * BLE), view device info, send AT commands, and chat with the on-device agent.
 */
export function UsbPanel() {
  const { theme } = useTheme();
  const isCoffee = theme === 'coffee';
  const [ports, setPorts] = useState<string[]>([]);
  const [selected, setSelected] = useState('');
  const [connected, setConnected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [info, setInfo] = useState<UsbDeviceInfo | null>(null);
  const [atCmd, setAtCmd] = useState('');
  const [atHistory, setAtHistory] = useState<AtItem[]>([]);
  const [chatText, setChatText] = useState('');
  const [chatMsgs, setChatMsgs] = useState<ChatMsg[]>([]);
  const [chatTyping, setChatTyping] = useState(false);
  const chatEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = chatEndRef.current;
    if (el && typeof el.scrollIntoView === 'function') {
      el.scrollIntoView({ behavior: 'smooth' });
    }
  }, [chatMsgs, chatTyping]);

  const label = {
    color: 'var(--color-text)',
    marginBottom: 4,
    fontSize: 12,
    fontWeight: 600,
  } as const;
  const input = {
    width: '100%',
    padding: '8px 10px',
    borderRadius: 8,
    border: '1px solid var(--color-border)',
    backgroundColor: isCoffee ? 'rgba(26,15,10,0.6)' : 'var(--color-bg)',
    color: 'var(--color-text)',
    outline: 'none',
  } as const;
  const box = {
    backgroundColor: isCoffee ? 'rgba(42,24,16,0.5)' : 'var(--color-surface)',
    border: '1px solid var(--color-border)',
    borderRadius: 12,
    padding: 16,
  } as const;

  const scanPorts = useCallback(async () => {
    setBusy(true);
    const p = await usbListPorts();
    setPorts(p);
    setStatus(p.length ? `找到 ${p.length} 个串口` : '未找到串口');
    setBusy(false);
  }, []);

  const connect = useCallback(async () => {
    if (!selected) return;
    setBusy(true);
    setStatus(`连接 ${selected}…`);
    try {
      await usbOpen(selected);
      setConnected(selected);
      setChatMsgs([{ role: 'system', text: `已通过 USB 连接到 ${selected}。发送消息即可与智能体对话。` }]);
      setAtHistory([]);
      await usbSendAt('AT');
      setStatus(`已连接 ${selected}`);
      const info = await usbDeviceInfo();
      setInfo(info);
      setAtHistory((h) => [{
        cmd: 'AT / 设备信息',
        resp: `OK · WiFi:${info.wifi ?? '?'} ${info.ip ?? ''}\n内存:${info.heap ?? '?'} · LLM:${info.llm ?? '?'}`,
      }, ...h]);
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [selected]);

  const refreshInfo = useCallback(async () => {
    setBusy(true);
    try {
      const info = await usbDeviceInfo();
      setInfo(info);
      setStatus('设备信息已刷新');
    } catch (e) {
      setStatus(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const disconnect = useCallback(async () => {
    await usbClose();
    setConnected(null);
    setInfo(null);
    setStatus('已断开');
    setChatMsgs([]);
    setAtHistory([]);
  }, []);

  const sendAt = useCallback(async () => {
    const cmd = atCmd.trim();
    if (!cmd) return;
    setBusy(true);
    try {
      const r = await usbSendAt(cmd);
      setAtHistory((h) => [{ cmd, resp: r.response }, ...h]);
      setAtCmd('');
    } catch (e) {
      setAtHistory((h) => [{ cmd, resp: e instanceof Error ? e.message : String(e) }, ...h]);
    } finally {
      setBusy(false);
    }
  }, [atCmd]);

  const chat = useCallback(async () => {
    const text = chatText.trim();
    if (!text) return;
    const now = () => new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
    setChatMsgs((m) => [...m, { role: 'user', text, time: now() }]);
    setChatText('');
    setChatTyping(true);
    try {
      const r = await usbAgentChat(text);
      setChatMsgs((m) => [...m, { role: 'agent', text: r.response, time: now() }]);
    } catch (e) {
      setChatMsgs((m) => [...m, { role: 'system', text: e instanceof Error ? e.message : String(e), time: now() }]);
    } finally {
      setChatTyping(false);
    }
  }, [chatText]);

  return (
    <div style={{ padding: 16, maxWidth: 760, margin: '0 auto', display: 'grid', gap: 14 }}>
      <div style={box}>
        <h2 style={{ margin: 0, fontSize: 16, color: 'var(--color-text)' }}>🔌 USB 连接 (C61)</h2>
        <p style={{ margin: '4px 0 12px', fontSize: 12, color: 'var(--color-text-muted)' }}>
          通过 USB 串口与设备通信（固件已关闭 BLE，改用 UART0 网关）
        </p>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
          <select value={selected} onChange={(e) => setSelected(e.target.value)} style={{ ...input, width: 240 }}>
            <option value="">选择串口…</option>
            {ports.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
          <button onClick={scanPorts} disabled={busy} style={btn(theme)}>🔍 扫描</button>
          {connected ? (
            <>
              <button onClick={refreshInfo} disabled={busy} style={btn(theme)}>刷新</button>
              <button onClick={disconnect} style={btn(theme, true)}>断开</button>
            </>
          ) : (
            <button onClick={connect} disabled={busy || !selected} style={btn(theme)}>连接</button>
          )}
        </div>
        {status && <p style={{ marginTop: 8, fontSize: 12, color: 'var(--color-text-muted)' }}>{status}</p>}
        {connected && <span style={{ fontSize: 12, color: 'var(--color-success)' }}>● 已连接 {connected}</span>}
      </div>
      {connected && (
        <>
          {/* Device info */}
          {info && (
            <div style={box}>
              <div style={label}>📊 设备信息</div>
              <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit,minmax(140px,1fr))', gap: 10 }}>
                {[
                  ['WiFi', info.wifi ?? '—'],
                  ['IP', info.ip ?? '—'],
                  ['内存', info.heap ?? '—'],
                  ['LLM', info.llm ?? '—'],
                  ['版本', info.version ?? '—'],
                  ['运行', info.uptime ?? '—'],
                ].map(([k, v]) => (
                  <div key={k} style={{ background: isCoffee ? 'rgba(26,15,10,0.4)' : 'var(--color-surface-hover)', borderRadius: 8, padding: '8px 10px' }}>
                    <div style={{ fontSize: 11, color: 'var(--color-text-muted)' }}>{k}</div>
                    <div style={{ fontSize: 13, color: 'var(--color-text)', fontWeight: 600, wordBreak: 'break-all' }}>{v}</div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* AT command */}
          <div style={box}>
            <div style={label}>AT 命令</div>
            <div style={{ display: 'flex', gap: 8 }}>
              <input
                value={atCmd}
                onChange={(e) => setAtCmd(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && sendAt()}
                placeholder="例如 AT+CWSTATE? / AT+HEAP"
                style={input}
              />
              <button onClick={sendAt} disabled={busy} style={btn(theme)}>发送</button>
            </div>
            {atHistory.length > 0 && (
              <div style={{ marginTop: 10, display: 'grid', gap: 6, maxHeight: 180, overflowY: 'auto' }}>
                {atHistory.map((it, i) => (
                  <div key={i} style={{ background: isCoffee ? 'rgba(26,15,10,0.4)' : 'var(--color-surface-hover)', borderRadius: 8, padding: 8, fontSize: 12 }}>
                    <span style={{ color: 'var(--color-primary)', fontWeight: 600 }}>&gt; {it.cmd}</span>
                    <pre style={{ margin: '4px 0 0', whiteSpace: 'pre-wrap', wordBreak: 'break-word', color: 'var(--color-text)' }}>{it.resp}</pre>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* Agent chat */}
          <div style={box}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <div style={label}>💬 与智能体对话</div>
              <button onClick={() => setChatMsgs([])} style={{ background: 'none', border: 'none', color: 'var(--color-text-muted)', cursor: 'pointer', fontSize: 12 }}>清空</button>
            </div>
            {/* Suggested prompts */}
            <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginBottom: 8 }}>
              {['读取温度', '内存还剩多少', '电池电量', '你是谁'].map((p) => (
                <button
                  key={p}
                  onClick={() => setChatText(p)}
                  style={{
                    background: isCoffee ? 'rgba(212,165,116,0.15)' : 'var(--color-surface-hover)',
                    border: '1px solid var(--color-border)',
                    color: 'var(--color-text)',
                    borderRadius: 999,
                    padding: '4px 10px',
                    fontSize: 12,
                    cursor: 'pointer',
                  }}
                >
                  {p}
                </button>
              ))}
            </div>
            <div style={{ maxHeight: 300, overflowY: 'auto', display: 'grid', gap: 10, padding: '4px 0' }}>
              {chatMsgs.map((m, i) => {
                const isUser = m.role === 'user';
                const isSys = m.role === 'system';
                return (
                  <div key={i} style={{ textAlign: isUser ? 'right' : 'left' }}>
                    <div
                      style={{
                        display: 'inline-block',
                        maxWidth: '85%',
                        padding: '8px 12px',
                        borderRadius: 12,
                        fontSize: 13,
                        whiteSpace: 'pre-wrap',
                        wordBreak: 'break-word',
                        background: isUser
                          ? 'var(--color-primary)'
                          : isSys
                            ? 'var(--color-warning-light)'
                            : (isCoffee ? 'rgba(212,165,116,0.25)' : 'var(--color-surface-hover)'),
                        color: isUser ? '#fff' : 'var(--color-text)',
                      }}
                    >
                      {m.text}
                    </div>
                    {m.time && (
                      <div
                        style={{
                          fontSize: 10,
                          marginTop: 2,
                          color: 'var(--color-text-muted)',
                          textAlign: isUser ? 'right' : 'left',
                        }}
                      >
                        {isUser ? '我' : isSys ? '系统' : '智能体'} · {m.time}
                      </div>
                    )}
                  </div>
                );
              })}
              {chatTyping && (
                <div style={{ textAlign: 'left', fontSize: 13, color: 'var(--color-text-muted)' }}>
                  <span style={{ display: 'inline-block', padding: '8px 12px', borderRadius: 12, background: isCoffee ? 'rgba(212,165,116,0.2)' : 'var(--color-surface-hover)' }}>智能体思考中…</span>
                </div>
              )}
              <div ref={chatEndRef} />
            </div>
            <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
              <input
                value={chatText}
                onChange={(e) => setChatText(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && chat()}
                placeholder="输入消息，例如：内存还剩多少 / 读取温度"
                style={input}
              />
              <button onClick={chat} disabled={busy || !chatText.trim()} style={btn(theme)}>发送</button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

function btn(theme: string, danger = false): React.CSSProperties {
  const primary = danger
    ? 'linear-gradient(135deg,#e06666,#c84a4a)'
    : theme === 'coffee'
      ? 'linear-gradient(135deg,#d4a574,#c8956a)'
      : 'var(--color-primary)';
  return {
    background: primary,
    color: theme === 'coffee' && !danger ? '#1a0f0a' : '#fff',
    border: 'none',
    borderRadius: 8,
    padding: '8px 14px',
    cursor: 'pointer',
    fontSize: 13,
    fontWeight: 600,
  };
}

