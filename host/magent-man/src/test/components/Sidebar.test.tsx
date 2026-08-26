import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { ThemeProvider } from '../../contexts/ThemeContext';
import { Sidebar, type NavItem } from '../../components/Sidebar';

// useAppInfo talks to the Tauri app plugin — mock it out.
vi.mock('@tauri-apps/api/app', () => ({
  getName: vi.fn().mockResolvedValue('mAgent-Man'),
  getVersion: vi.fn().mockResolvedValue('0.2.0'),
}));

const ALL_NAV: NavItem[] = ['config', 'chat', 'channels', 'status', 'advanced'];

async function renderSidebar(activeNav: NavItem = 'chat') {
  const onNavChange = vi.fn();
  const utils = render(
    <ThemeProvider>
      <Sidebar activeNav={activeNav} onNavChange={onNavChange} />
    </ThemeProvider>
  );
  // Flush the async useAppInfo effect (getName/getVersion) so it doesn't
  // trigger a state update outside of act().
  await act(async () => {});
  return { onNavChange, ...utils };
}

describe('Sidebar', () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.className = '';
  });

  describe('branding', () => {
    it('renders the app name and subtitle', async () => {
      await renderSidebar();
      expect(screen.getByText('mAgent-Man')).toBeInTheDocument();
      expect(screen.getByText('app.subtitle')).toBeInTheDocument();
    });

    it('shows the version in the footer', async () => {
      await renderSidebar();
      expect(screen.getByText(/v0\.2\.0/)).toBeInTheDocument();
    });
  });

  describe('navigation', () => {
    it('renders every navigation item', async () => {
      await renderSidebar();
      ALL_NAV.forEach((item) => {
        expect(screen.getByText(`nav.${item}`)).toBeInTheDocument();
      });
    });

    it('calls onNavChange with the clicked item id', async () => {
      const { onNavChange } = await renderSidebar('chat');
      ALL_NAV.forEach((item) => {
        fireEvent.click(screen.getByText(`nav.${item}`));
        expect(onNavChange).toHaveBeenLastCalledWith(item);
      });
      expect(onNavChange).toHaveBeenCalledTimes(ALL_NAV.length);
    });

    it('places Configuration (参数配置) at the bottom of the nav list', async () => {
      await renderSidebar();
      const labels = screen.getAllByText(/^nav\./);
      expect(labels).toHaveLength(ALL_NAV.length);
      // "config" must be the very last nav item.
      expect(labels[labels.length - 1]).toHaveTextContent('nav.config');
    });
  });

  describe('collapse', () => {
    it('renders a collapse button when onCollapse is provided', async () => {
      const onCollapse = vi.fn();
      render(
        <ThemeProvider>
          <Sidebar activeNav="chat" onNavChange={vi.fn()} onCollapse={onCollapse} />
        </ThemeProvider>
      );
      await act(async () => {});
      fireEvent.click(screen.getByRole('button', { name: /nav\.collapse/ }));
      expect(onCollapse).toHaveBeenCalledTimes(1);
    });

    it('does not render a collapse button without onCollapse', async () => {
      await renderSidebar();
      expect(screen.queryByRole('button', { name: /nav\.collapse/ })).not.toBeInTheDocument();
    });
  });
});

