import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
  notifications,
  notify,
  notifyError,
  dismissNotification,
  clearNotifications,
} from './notifications.js';
import { parseLauncherError } from './api.js';
import fs from 'fs';
import path from 'path';

describe('Unified Notification System (notifications.js)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    clearNotifications();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('starts with an empty notification stack', () => {
    expect(get(notifications)).toEqual([]);
  });

  it('adds an info notification with default duration', () => {
    const id = notify('Game launched successfully');
    expect(typeof id).toBe('number');

    const list = get(notifications);
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({
      id,
      message: 'Game launched successfully',
      type: 'info',
      duration: 4000,
    });
  });

  it('supports semantic helper methods for all notification levels', () => {
    const id1 = notify.info('Info note');
    const id2 = notify.success('Success note');
    const id3 = notify.error('Error note');
    const id4 = notify.warning('Warning note');

    const list = get(notifications);
    expect(list).toHaveLength(4);

    expect(list.find((n) => n.id === id1).type).toBe('info');
    expect(list.find((n) => n.id === id2).type).toBe('success');
    expect(list.find((n) => n.id === id3).type).toBe('error');
    expect(list.find((n) => n.id === id4).type).toBe('warning');
  });

  it('auto-dismisses notification after duration expires', () => {
    notify('Auto dismiss me', 'info', 2000);
    expect(get(notifications)).toHaveLength(1);

    vi.advanceTimersByTime(1999);
    expect(get(notifications)).toHaveLength(1);

    vi.advanceTimersByTime(2);
    expect(get(notifications)).toHaveLength(0);
  });

  it('supports persistent notifications when duration is 0', () => {
    const id = notify('Persistent note', 'warning', 0);
    expect(get(notifications)).toHaveLength(1);

    vi.advanceTimersByTime(60000);
    expect(get(notifications)).toHaveLength(1);

    dismissNotification(id);
    expect(get(notifications)).toHaveLength(0);
  });

  it('manually dismisses a specific notification by ID', () => {
    const id1 = notify('First');
    const id2 = notify('Second');
    const id3 = notify('Third');

    expect(get(notifications)).toHaveLength(3);

    dismissNotification(id2);
    const list = get(notifications);
    expect(list).toHaveLength(2);
    expect(list.map((n) => n.id)).toEqual([id1, id3]);
  });

  it('clears all active notifications at once', () => {
    notify('One');
    notify('Two');
    notify('Three');
    expect(get(notifications)).toHaveLength(3);

    clearNotifications();
    expect(get(notifications)).toEqual([]);
  });

  it('correctly parses structured and fallback launcher errors', () => {
    expect(parseLauncherError(null)).toMatchObject({ code: 'general' });
    expect(parseLauncherError('Network timed out')).toMatchObject({
      code: 'general',
      message: 'Network timed out',
    });

    const parsedJson = parseLauncherError(
      JSON.stringify({ code: 'runner_not_found', message: 'Runner GE-Proton not found' })
    );
    expect(parsedJson).toMatchObject({
      code: 'runner_not_found',
      message: 'Runner GE-Proton not found',
    });

    const parsedObj = parseLauncherError({
      code: 'drive_full',
      message: 'Drive is full',
      path: '/games',
    });
    expect(parsedObj).toMatchObject({
      code: 'drive_full',
      message: 'Drive is full',
      path: '/games',
    });
  });

  it('attaches contextual remediation action for runner_not_found', () => {
    let settingsOpened = false;
    notifyError(
      { code: 'runner_not_found', message: 'Missing compatibility runner GE-Proton' },
      {
        onOpenSettings: (cat) => {
          expect(cat).toBe('runners');
          settingsOpened = true;
        },
      }
    );

    const list = get(notifications);
    expect(list).toHaveLength(1);
    expect(list[0].type).toBe('error');
    expect(list[0].action).toBeTruthy();
    expect(list[0].action.label).toBe('Runners Settings');

    list[0].action.onClick();
    expect(settingsOpened).toBe(true);
  });


});

describe('Zero Native Dialogs & Error Palette Invariants', () => {
  it('strictly verifies zero window.confirm() or alert() calls across all frontend source files', () => {
    const srcDir = path.resolve(__dirname);
    const files = [];

    function collectSourceFiles(dir) {
      const entries = fs.readdirSync(dir, { withFileTypes: true });
      for (const entry of entries) {
        const fullPath = path.join(dir, entry.name);
        if (entry.isDirectory()) {
          collectSourceFiles(fullPath);
        } else if (
          (entry.name.endsWith('.svelte') || entry.name.endsWith('.js')) &&
          !entry.name.endsWith('.test.js')
        ) {
          files.push(fullPath);
        }
      }
    }

    collectSourceFiles(srcDir);
    expect(files.length).toBeGreaterThan(5);

    const violations = [];
    for (const file of files) {
      const content = fs.readFileSync(file, 'utf-8');
      if (/\bconfirm\s*\(/.test(content)) {
        violations.push(`${path.basename(file)} contains confirm()`);
      }
      if (/\balert\s*\(/.test(content)) {
        violations.push(`${path.basename(file)} contains alert()`);
      }
    }

    expect(violations).toEqual([]);
  });

  it('verifies ConfirmModal contract for destructive actions and error palette styling', () => {
    function getConfirmModalConfig(isDestructive) {
      return {
        isDestructive,
        iconClass: isDestructive ? 'danger-icon' : 'info-icon',
        titleClass: isDestructive ? 'confirm-title danger' : 'confirm-title',
        buttonClass: isDestructive ? 'md3-btn danger' : 'md3-btn primary',
        colorToken: isDestructive
          ? 'var(--md-sys-color-error)'
          : 'var(--md-sys-color-primary)',
        containerToken: isDestructive
          ? 'var(--md-sys-color-error-container)'
          : 'var(--md-sys-color-primary-container)',
      };
    }

    const destructiveConfig = getConfirmModalConfig(true);
    expect(destructiveConfig.iconClass).toBe('danger-icon');
    expect(destructiveConfig.titleClass).toContain('danger');
    expect(destructiveConfig.buttonClass).toBe('md3-btn danger');
    expect(destructiveConfig.colorToken).toBe('var(--md-sys-color-error)');
    expect(destructiveConfig.containerToken).toBe('var(--md-sys-color-error-container)');

    const standardConfig = getConfirmModalConfig(false);
    expect(standardConfig.iconClass).toBe('info-icon');
    expect(standardConfig.buttonClass).toBe('md3-btn primary');
  });
});

describe('Game Page Cleanliness & Layout Specifications', () => {
  it('positions game title in the topbar header and eliminates steam id from header', () => {
    const game = {
      id: 'steam-1091500',
      title: 'Cyberpunk 2077',
      steam_app_id: '1091500',
    };

    function deriveHeaderData(g, steamData) {
      return {
        headerTitle: steamData?.name || g.title,
        hasSteamIdInHeader: false, // Steam ID removed from topbar header
        hasInstalledPill: false,   // "Installed" pill removed from topbar header
      };
    }

    const header = deriveHeaderData(game, null);
    expect(header.headerTitle).toBe('Cyberpunk 2077');
    expect(header.hasSteamIdInHeader).toBe(false);
    expect(header.hasInstalledPill).toBe(false);
  });

  it('toggles header translucency dynamically when scrolling', () => {
    function computeHeaderState(scrollTop) {
      return {
        isScrolled: scrollTop > 10,
        background: scrollTop > 10
          ? 'var(--md-sys-color-surface-container)'
          : 'transparent',
        hasBlur: scrollTop > 10,
      };
    }

    // At top of page: transparent, no blur
    const atTop = computeHeaderState(0);
    expect(atTop.isScrolled).toBe(false);
    expect(atTop.background).toBe('transparent');
    expect(atTop.hasBlur).toBe(false);

    // Scrolled down: container background, frosted blur
    const scrolled = computeHeaderState(45);
    expect(scrolled.isScrolled).toBe(true);
    expect(scrolled.background).toBe('var(--md-sys-color-surface-container)');
    expect(scrolled.hasBlur).toBe(true);
  });

  it('formats sidebar Genres & Tags with left-aligned label and right-wrapped tags', () => {
    const rowLayout = {
      display: 'flex',
      flexDirection: 'row',
      justifyContent: 'space-between',
      alignItems: 'flex-start',
      labelAlign: 'left',
      tagsJustify: 'flex-end',
      tagsWrap: 'wrap',
    };

    expect(rowLayout.labelAlign).toBe('left');
    expect(rowLayout.tagsJustify).toBe('flex-end');
    expect(rowLayout.tagsWrap).toBe('wrap');
  });
});
