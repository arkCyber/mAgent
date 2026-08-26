import { useState, useCallback, useEffect, useRef } from 'react';
import { useTheme } from '../contexts/ThemeContext';
import {
  usbListPorts,
  usbOpen,
  usbClose,
  usbSendAt,
  usbAgentChat,
  usbGetStatus,
} from '../hooks/useUsb';

interface Msg {
  role: 'user' | 'agent' | 'system';
  text: string;
  time?: string;
}

interface UsbChatPanelProps {
  /** App-level USB connection state (from the startup auto-connect). */
  autoConnected?: boolean;
  autoPath?: string | null;
}

/**
 * USB chat tab: connect to the C61 over USB and chat with the on-device agent.
 * Manages its own connection for manual use, and reflects the app-level
 * auto-connect when `autoConnected`/`autoPath` props are provided.
 */
export function UsbChatPanel({ autoConnected, autoPath }: UsbChatPanelProps) {
  const { theme } = useTheme();
  const isCoffee = theme === 'coffee';
  const [connected, setConnected] = useState(false);
  const [path, setPath] = useState<string | null>(null);
  const [ports, setPorts] = useState<string[]>([]);
  const [selected, setSelected] = useState('');
  const [manual, setManual] = useState('');
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [msgs, setMsgs] = useState<Msg[]>([]);
  const [text, setText] = useState('');
  const [typing, setTyping] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);

  // Reflect the app-level USB auto-connect (startup) when it completes.
  useEffect(() => {
    if (autoConnected && autoPath) {
      setConnected(true);
      setPath(autoPath);
      setStatus(`已连接 ${autoPath}`);
      setMsgs((prev) =>
        prev.length
          ? prev
          : [{ role: 'system', text: `已通过 USB 连接到 ${autoPath}。发送消息即可与智能体对话。` }]
      );
    }
  }, [autoConnected, autoPath]);

  useEffect(() => {
    (async () => {
      const st = await usbGetStatus();
      if (st.connected) {
        setConnected(true);
        setPath(st.path);
        setStatus(`已连接 ${st.path}`);
        setMsgs([{ role: 'system', text: `已通过 USB 连接到 ${st.path}。发送消息即可与智能体对话。` }]);
      }
    })();
  }, []);

  useEffect(() => {
    const el = endRef.current;
    if (el && typeof el.scrollIntoView === 'function') {
      el.scrollIntoView({ behavior: 'smooth' });
    }
  }, [msgs, typing]);

  const now = () => new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });

  const scanPorts = useCallback(async () => {
    setBusy(true);
    const p = await usbListPorts();
    setPorts(p);
    setStatus(p.length ? `找到 ${p.length} 个串口` : '未找到串口，请确认 USB 已连接');
    setBusy(false);
  }, []);

  const connect = useCallback(async () => {
    const port = selected || manual;
    if (!port) return;
    setBusy(true);
    setStatus(`连接 ${port}…`);
    try {
      await usbOpen(port);
      // Probe the link with a fast AT to confirm the device is responsive.
      const probe = await usbSendAt('AT');
      setConnected(true);
      setPath(port);
      setStatus(`✓ 连接成功：${port}`);
      setMsgs([
        { role: 'system', text: `已通过 USB 连接到 ${port}，链路探测 OK${probe.response.trim() ? `（${probe.response.trim()}）` : ''}。发送消息即可与智能体对话。` },
      ]);
    } catch (e) {
      setConnected(false);
      setStatus(`连接失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [selected, manual]);

  const disconnect = useCallback(async () => {
    await usbClose();
    setConnected(false);
    setPath(null);
    setStatus('已断开');
    setMsgs([]);
  }, []);

  const send = useCallback(async () => {
    const t = text.trim();
    if (!t) return;
    setMsgs((m) => [...m, { role: 'user', text: t, time: now() }]);
    setText('');
    setTyping(true);
    try {
      const r = await usbAgentChat(t);
      setMsgs((m) => [...m, { role: 'agent', text: r.response, time: now() }]);
    } catch (e) {
      setMsgs((m) => [...m, { role: 'system', text: e instanceof Error ? e.message : String(e), time: now() }]);
    } finally {
      setTyping(false);
    }
  }, [text]);

  const input = {
    width: '100%',
    padding: '10px 12px',
    borderRadius: 10,
    border: '1px solid var(--color-border)',
    backgroundColor: isCoffee ? 'rgba(26,15,10,0.6)' : 'var(--color-bg)',
    color: 'var(--color-text)',
    outline: 'none',
  } as const;

  if (!connected) {
    return (
      <div style={{ maxWidth: 640, margin: '0 auto', padding: 24, textAlign: 'center' }}>
        <div style={{ fontSize: 40 }}>🔌</div>
        <h2 style={{ color: 'var(--color-text)', marginBottom: 6 }}>与智能体对话</h2>
        <p style={{ color: 'var(--color-text-muted)', fontSize: 13, marginBottom: 20 }}>
          通过 USB 串口连接 C61，与本地智能体对话（支持中英文）
        </p>
        <div style={{ display: 'flex', gap: 8, justifyContent: 'center', flexWrap: 'wrap' }}>
          <select
            value={selected}
            onChange={(e) => { setSelected(e.target.value); setManual(''); }}
            style={{ ...input, width: 260, textAlign: 'left' }}
          >
            <option value="">选择串口…</option>
            {ports.map((p) => (
              <option key={p} value={p}>{p}</option>
            ))}
          </select>
          <button onClick={scanPorts} disabled={busy} style={btn(theme)}>🔍 扫描</button>
          <button onClick={connect} disabled={busy || (!selected && !manual)} style={btn(theme)}>连接</button>
        </div>
        <div style={{ marginTop: 10, display: 'flex', gap: 8, justifyContent: 'center' }}>
          <input
            value={manual}
            onChange={(e) => { setManual(e.target.value); setSelected(''); }}
            placeholder="或手动输入串口，如 /dev/cu.usbserial-10"
            style={{ ...input, width: 300 }}
          />
        </div>
        {status && (
          <p
            style={{
              marginTop: 12,
              fontSize: 13,
              color: status.startsWith('✓') ? 'var(--color-success)' : status.startsWith('连接失败') ? 'var(--color-error)' : 'var(--color-text-muted)',
              fontWeight: status.startsWith('✓') ? 700 : 400,
            }}
          >
            {status}
          </p>
        )}
        <p style={{ marginTop: 20, color: 'var(--color-text-muted)', fontSize: 11 }}>
          首次使用：把 C61 用 USB 线连到电脑，点击"扫描"找到 /dev/cu.usbserial-* 后连接；或手动输入串口路径。
        </p>
      </div>
    );
  }
  // ---- Connected: chat ----
  return (
    <div style={{ maxWidth: 720, margin: '0 auto', padding: 16, display: 'grid', gap: 12 }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <div>
          <h2 style={{ margin: 0, fontSize: 17, color: 'var(--color-text)' }}>💬 智能体对话</h2>
          <span style={{ fontSize: 12, color: 'var(--color-success)' }}>● {path}</span>
        </div>
        <button onClick={disconnect} style={btn(theme, true)}>断开</button>
      </div>

      <div
        style={{
          height: 'calc(100vh - 280px)',
          minHeight: 320,
          overflowY: 'auto',
          display: 'grid',
          gap: 10,
          padding: 14,
          borderRadius: 14,
          background: isCoffee ? 'rgba(26,15,10,0.4)' : 'var(--color-surface)',
          border: '1px solid var(--color-border)',
        }}
      >
        {msgs.map((m, i) => {
          const isUser = m.role === 'user';
          const isSys = m.role === 'system';
          return (
            <div key={i} style={{ textAlign: isUser ? 'right' : 'left' }}>
              <div
                style={{
                  display: 'inline-block',
                  maxWidth: '80%',
                  padding: '9px 13px',
                  borderRadius: 14,
                  fontSize: 14,
                  lineHeight: 1.5,
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
                <div style={{ fontSize: 10, marginTop: 2, color: 'var(--color-text-muted)', textAlign: isUser ? 'right' : 'left' }}>
                  {isUser ? '我' : isSys ? '系统' : '智能体'} · {m.time}
                </div>
              )}
            </div>
          );
        })}
        {typing && (
          <div style={{ textAlign: 'left' }}>
            <span style={{ display: 'inline-block', padding: '9px 13px', borderRadius: 14, background: isCoffee ? 'rgba(212,165,116,0.2)' : 'var(--color-surface-hover)', fontSize: 13, color: 'var(--color-text-muted)' }}>智能体思考中…</span>
          </div>
        )}
        <div ref={endRef} />
      </div>

      <div>
        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', marginBottom: 8 }}>
          {['读取温度', '内存还剩多少', '电池电量', '你是谁'].map((p) => (
            <button key={p} onClick={() => setText(p)} style={chip(isCoffee)}>{p}</button>
          ))}
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <input
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && send()}
            placeholder="输入消息，例如：内存还剩多少"
            style={input}
          />
          <button onClick={send} disabled={busy || !text.trim()} style={btn(theme)}>发送</button>
        </div>
      </div>
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
    borderRadius: 10,
    padding: '10px 16px',
    cursor: 'pointer',
    fontSize: 14,
    fontWeight: 600,
    whiteSpace: 'nowrap',
  };
}

function chip(isCoffee: boolean): React.CSSProperties {
  return {
    background: isCoffee ? 'rgba(212,165,116,0.15)' : 'var(--color-surface-hover)',
    border: '1px solid var(--color-border)',
    color: 'var(--color-text)',
    borderRadius: 999,
    padding: '5px 12px',
    fontSize: 12,
    cursor: 'pointer',
  };
}

