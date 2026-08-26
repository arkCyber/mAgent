import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { ThemeProvider } from '../../contexts/ThemeContext';
import { TopBarSettings } from '../../components/TopBarSettings';

// useAppInfo talks to the Tauri app plugin — mock it out.
vi.mock('@tauri-apps/api/app', () => ({
  getName: vi.fn().mockResolvedValue('mAgent-Man'),
  getVersion: vi.fn().mockResolvedValue('0.2.0'),
}));

// A single shared changeLanguage spy so the component and the test observe the
// same instance (the global setup mock creates a fresh fn per hook call).
const { changeLanguageMock } = vi.hoisted(() => ({ changeLanguageMock: vi.fn() }));

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, options?: { [key: string]: string | number }) => {
      let result = key;
      if (options) {
        Object.entries(options).forEach(([k, v]) => {
          result = result.replace(new RegExp(`\\{${k}\\}`, 'g'), String(v));
        });
      }
      return result;
    },
    i18n: { language: 'en', changeLanguage: changeLanguageMock },
  }),
  initReactI18next: { type: '3rdParty', init: vi.fn() },
}));

async function renderTopBarSettings() {
  const utils = render(
    <ThemeProvider>
      <TopBarSettings />
    </ThemeProvider>
  );
  await act(async () => {});
  return utils;
}

describe('TopBarSettings', () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.className = '';
    changeLanguageMock.mockClear();
  });

  describe('theme switch', () => {
    it('renders a theme button in the top bar', async () => {
      await renderTopBarSettings();
      expect(screen.getByRole('button', { name: 'settings.theme' })).toBeInTheDocument();
    });

    it('opens the theme menu and switches the theme', async () => {
      await renderTopBarSettings();
      fireEvent.click(screen.getByRole('button', { name: 'settings.theme' }));
      // All four themes are available (names are localized via t()).
      expect(screen.getByRole('button', { name: /settings\.darkMode/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /settings\.lightMode/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /settings\.coffeeMode/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /settings\.warmMode/ })).toBeInTheDocument();

      fireEvent.click(screen.getByRole('button', { name: /settings\.darkMode/ }));
      expect(document.documentElement.classList.contains('dark')).toBe(true);
      expect(localStorage.getItem('theme')).toBe('dark');
    });
  });

  describe('language dropdown', () => {
    it('renders a language button showing the current locale', async () => {
      await renderTopBarSettings();
      const langBtn = screen.getByRole('button', { name: 'settings.language' });
      expect(langBtn).toBeInTheDocument();
      expect(langBtn.textContent).toContain('en');
    });

    it('lists the supported languages', async () => {
      await renderTopBarSettings();
      fireEvent.click(screen.getByRole('button', { name: 'settings.language' }));
      expect(screen.getByRole('button', { name: /English/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /简体中文/ })).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /繁體中文/ })).toBeInTheDocument();
    });

    it('calls changeLanguage and persists the selection', async () => {
      await renderTopBarSettings();
      fireEvent.click(screen.getByRole('button', { name: 'settings.language' }));
      fireEvent.click(screen.getByRole('button', { name: /简体中文/ }));
      expect(changeLanguageMock).toHaveBeenCalledWith('zh');
      expect(localStorage.getItem('language')).toBe('zh');
    });
  });
});
