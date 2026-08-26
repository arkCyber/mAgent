import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useTheme } from '../contexts/ThemeContext';
import { useAppInfo } from '../hooks/useAppInfo';

const languages = [
  { code: 'en', name: 'English', flag: '🇺🇸' },
  { code: 'zh', name: '简体中文', flag: '🇨🇳' },
  { code: 'zh-TW', name: '繁體中文', flag: '🇹🇼' },
];

const themes = [
  { id: 'light', icon: '☀️', nameKey: 'settings.lightMode' },
  { id: 'dark', icon: '🌙', nameKey: 'settings.darkMode' },
  { id: 'warm', icon: '🌅', nameKey: 'settings.warmMode' },
  { id: 'coffee', icon: '☕', nameKey: 'settings.coffeeMode' },
] as const;

/**
 * App-level controls shown in the top-right corner of the window:
 * a theme switch (light/dark and friends) and a language selection dropdown.
 */
export function TopBarSettings() {
  const { t, i18n } = useTranslation();
  const { theme, setTheme } = useTheme();
  const appInfo = useAppInfo();
  const [themeOpen, setThemeOpen] = useState(false);
  const [langOpen, setLangOpen] = useState(false);

  const currentLang = languages.find((l) => l.code === i18n.language) || languages[0];
  const currentTheme = themes.find((th) => th.id === theme) || themes[0];

  const changeLanguage = (code: string) => {
    i18n.changeLanguage(code);
    localStorage.setItem('language', code);
    setLangOpen(false);
  };

  return (
    <div className="flex items-center gap-2">
      {/* Theme switch */}
      <div className="relative">
        <button
          onClick={() => setThemeOpen((o) => !o)}
          title={t('settings.theme')}
          aria-label={t('settings.theme')}
          aria-expanded={themeOpen}
          aria-controls="topbar-theme-menu"
          className="flex items-center justify-center w-9 h-9 rounded-lg bg-white/10 hover:bg-white/20 transition-all duration-200 hover:scale-105 text-lg"
        >
          {currentTheme.icon}
        </button>

        {themeOpen && (
          <>
            <div className="fixed inset-0 z-40" onClick={() => setThemeOpen(false)} />
            <div
              id="topbar-theme-menu"
              className="absolute right-0 mt-2 w-48 rounded-xl shadow-xl border overflow-hidden z-50 animate-fade-in"
              style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}
            >
              <p
                className="px-4 pt-3 pb-1 text-xs font-semibold uppercase tracking-wider"
                style={{ color: 'var(--color-text-muted)' }}
              >
                {t('settings.theme')}
              </p>
              <div className="grid grid-cols-2 gap-1 p-2">
                {themes.map((th) => (
                  <button
                    key={th.id}
                    onClick={() => setTheme(th.id)}
                    className="flex flex-col items-center gap-1 p-2 rounded-lg transition-all duration-200"
                    style={{
                      backgroundColor:
                        theme === th.id ? 'var(--color-primary-light)' : 'var(--color-surface-hover)',
                    }}
                    aria-pressed={theme === th.id}
                  >
                    <span className="text-lg">{th.icon}</span>
                    <span
                      className="text-xs font-medium"
                      style={{ color: theme === th.id ? 'var(--color-primary)' : 'var(--color-text-secondary)' }}
                    >
                      {t(th.nameKey)}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          </>
        )}
      </div>

      {/* Language dropdown */}
      <div className="relative">
        <button
          onClick={() => setLangOpen((o) => !o)}
          title={t('settings.language')}
          aria-label={t('settings.language')}
          aria-expanded={langOpen}
          aria-controls="topbar-lang-menu"
          className="flex items-center justify-center gap-1 px-3 h-9 rounded-lg bg-white/10 hover:bg-white/20 transition-all duration-200 hover:scale-105"
        >
          <span className="text-base">{currentLang.flag}</span>
          <span className="text-xs text-white/80">{currentLang.code}</span>
        </button>

        {langOpen && (
          <>
            <div className="fixed inset-0 z-40" onClick={() => setLangOpen(false)} />
            <div
              id="topbar-lang-menu"
              className="absolute right-0 mt-2 w-56 rounded-xl shadow-xl border overflow-hidden z-50 animate-fade-in"
              style={{ backgroundColor: 'var(--color-surface)', borderColor: 'var(--color-border)' }}
            >
              <p
                className="px-4 pt-3 pb-1 text-xs font-semibold uppercase tracking-wider"
                style={{ color: 'var(--color-text-muted)' }}
              >
                {t('settings.language')}
              </p>
              <div className="p-2 space-y-1">
                {languages.map((lang) => (
                  <button
                    key={lang.code}
                    onClick={() => changeLanguage(lang.code)}
                    className="w-full flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-all duration-200"
                    style={{
                      backgroundColor:
                        i18n.language === lang.code ? 'var(--color-primary-light)' : 'transparent',
                      color: i18n.language === lang.code ? 'var(--color-primary)' : 'var(--color-text)',
                    }}
                    aria-pressed={i18n.language === lang.code}
                  >
                    <span className="text-lg">{lang.flag}</span>
                    <span className="flex-1 text-left font-medium">{lang.name}</span>
                    {i18n.language === lang.code && (
                      <span style={{ color: 'var(--color-primary)' }}>✓</span>
                    )}
                  </button>
                ))}
              </div>
              <div
                className="px-4 py-2 text-center"
                style={{ borderTop: '1px solid var(--color-border)' }}
              >
                <span
                  className="text-xs font-mono"
                  style={{ color: 'var(--color-text-muted)' }}
                >
                  {appInfo.name} v{appInfo.version}
                </span>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

