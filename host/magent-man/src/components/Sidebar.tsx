import { useTranslation } from 'react-i18next';
import { useTheme } from '../contexts/ThemeContext';
import { useAppInfo } from '../hooks/useAppInfo';

export type NavItem = 'config' | 'chat' | 'channels' | 'status' | 'advanced';

interface SidebarProps {
  activeNav: NavItem;
  onNavChange: (nav: NavItem) => void;
  /** Hide the sidebar (collapse it) — the app shows a floating expand button. */
  onCollapse?: () => void;
}

interface NavItemConfig {
  id: NavItem;
  icon: string;
  labelKey: string;
  badge?: number;
}

// 参数配置 (Configuration) is intentionally placed at the BOTTOM of the nav
// list so the primary screens sit at the top and config stays one click away.
const navItems: NavItemConfig[] = [
  { id: 'chat', icon: '💬', labelKey: 'nav.chat' },
  { id: 'channels', icon: '🔗', labelKey: 'nav.channels' },
  { id: 'status', icon: '📊', labelKey: 'nav.status' },
  { id: 'advanced', icon: '🔧', labelKey: 'nav.advanced' },
  { id: 'config', icon: '⚙️', labelKey: 'nav.config' },
];

export function Sidebar({ activeNav, onNavChange, onCollapse }: SidebarProps) {
  const { t } = useTranslation();
  const { theme } = useTheme();
  const appInfo = useAppInfo();

  const getSidebarBg = () => {
    switch (theme) {
      case 'coffee':
        return 'rgba(42, 24, 16, 0.95)';
      case 'dark':
        return '#1a1a2e';
      case 'warm':
        return '#fef7ed';
      default:
        return 'rgba(255, 255, 255, 0.95)';
    }
  };

  const getAccentColor = () => {
    switch (theme) {
      case 'coffee':
        return '#d4a574';
      default:
        return 'var(--color-primary)';
    }
  };

  const getTextColor = () => {
    switch (theme) {
      case 'coffee':
      case 'dark':
        return '#f5f5f5';
      default:
        return 'var(--color-text)';
    }
  };

  const getMutedTextColor = () => {
    switch (theme) {
      case 'coffee':
        return 'rgba(245, 245, 245, 0.5)';
      case 'dark':
        return 'rgba(245, 245, 245, 0.5)';
      default:
        return 'var(--color-text-muted)';
    }
  };

  const accentColor = getAccentColor();
  const textColor = getTextColor();
  const mutedTextColor = getMutedTextColor();

  return (
    <aside
      className="flex flex-col h-full"
      style={{
        backgroundColor: getSidebarBg(),
        width: '220px',
        backdropFilter: 'blur(12px)',
      }}
    >
      {/* Logo / Brand */}
      <div
        className="flex items-center gap-3 px-5 py-5"
        style={{ borderBottom: '1px solid rgba(128, 128, 128, 0.2)' }}
      >
        <div
          className="w-10 h-10 rounded-xl flex items-center justify-center shadow-md"
          style={{
            background: `linear-gradient(135deg, ${accentColor}, ${accentColor}aa)`,
          }}
        >
          <span className="text-xl">🤖</span>
        </div>
        <div className="flex flex-col min-w-0 flex-1">
          <span
            className="font-bold tracking-tight truncate"
            style={{ color: textColor, fontSize: '15px' }}
          >
            {appInfo.name}
          </span>
          <span
            className="text-xs truncate"
            style={{ color: mutedTextColor }}
          >
            {t('app.subtitle')}
          </span>
        </div>
        {onCollapse && (
          <button
            onClick={onCollapse}
            title={t('nav.collapse')}
            aria-label={t('nav.collapse')}
            className="flex items-center justify-center w-7 h-7 rounded-lg transition-all duration-200 hover:scale-110 flex-shrink-0"
            style={{ color: mutedTextColor, backgroundColor: `${accentColor}14` }}
          >
            ‹
          </button>
        )}
      </div>

      {/* Navigation */}
      <nav className="flex-1 px-3 py-4 space-y-1 overflow-y-auto">
        {navItems.map((item) => {
          const isActive = activeNav === item.id;
          return (
            <button
              key={item.id}
              onClick={() => onNavChange(item.id)}
              className="w-full flex items-center gap-3 px-4 py-3 rounded-xl text-sm font-medium transition-all duration-200 group"
              style={{
                backgroundColor: isActive
                  ? accentColor
                  : 'transparent',
                color: isActive
                  ? theme === 'coffee' ? '#1a0f0a' : '#ffffff'
                  : mutedTextColor,
                boxShadow: isActive
                  ? `0 4px 16px ${accentColor}33`
                  : 'none',
              }}
              onMouseEnter={(e) => {
                if (!isActive) {
                  e.currentTarget.style.backgroundColor = `${accentColor}18`;
                  e.currentTarget.style.color = textColor;
                }
              }}
              onMouseLeave={(e) => {
                if (!isActive) {
                  e.currentTarget.style.backgroundColor = 'transparent';
                  e.currentTarget.style.color = mutedTextColor;
                }
              }}
            >
              <span
                className="text-lg flex-shrink-0 transition-transform duration-200 group-hover:scale-110"
                style={{ transform: isActive ? 'scale(1.1)' : 'scale(1)' }}
              >
                {item.icon}
              </span>
              <span className="flex-1 text-left truncate">
                {t(item.labelKey)}
              </span>
              {item.badge !== undefined && item.badge > 0 && (
                <span
                  className="flex items-center justify-center min-w-[20px] h-5 px-1.5 rounded-full text-xs font-bold"
                  style={{
                    backgroundColor: isActive
                      ? theme === 'coffee' ? '#1a0f0a' : '#ffffff'
                      : accentColor,
                    color: isActive
                      ? accentColor
                      : theme === 'coffee' ? '#1a0f0a' : '#ffffff',
                  }}
                >
                  {item.badge > 99 ? '99+' : item.badge}
                </span>
              )}
            </button>
          );
        })}
      </nav>

      {/* Version Footer */}
      <div
        className="px-5 py-3 text-center"
        style={{ borderTop: '1px solid rgba(128, 128, 128, 0.2)' }}
      >
        <span
          className="text-xs font-mono"
          style={{ color: mutedTextColor }}
        >
          {appInfo.name} v{appInfo.version}
        </span>
      </div>
    </aside>
  );
}
