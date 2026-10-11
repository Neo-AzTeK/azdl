import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import * as api from './api.js';

// Mock @tauri-apps/api/core
const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args) => mockInvoke(...args),
  isTauri: () => false,
}));

describe('API - Browser Fallback Mode', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    delete global.window;
  });

  it('detects isDesktop as false in non-Tauri environment', () => {
    expect(api.isDesktop()).toBe(false);
  });

  it('returns valid fallback state on getState', async () => {
    const state = await api.getState();
    expect(state).toHaveProperty('library');
    expect(Array.isArray(state.library)).toBe(true);
    expect(state).toHaveProperty('sources');
    expect(state).toHaveProperty('jobs');
    expect(state).toHaveProperty('running_games');
  });

  it('returns fallback runners', async () => {
    const runners = await api.getRunners();
    expect(runners.length).toBeGreaterThanOrEqual(3);
    expect(runners.some((r) => r.id === 'auto')).toBe(true);
  });

  it('returns mock responses for launch and stop', async () => {
    const launchRes = await api.launchGame('test-game', ['-flag']);
    expect(launchRes).toEqual({ status: 'mock_launched', id: 'test-game' });

    const stopRes = await api.stopGame('test-game');
    expect(stopRes).toEqual({ status: 'mock_stopped', id: 'test-game' });
  });

  it('returns mock response for runAction', async () => {
    const res = await api.runAction({ action: 'test' });
    expect(res).toEqual({ success: true });
  });

  it('returns fallback false for isSteamRunning and true for startSteam', async () => {
    expect(await api.isSteamRunning()).toBe(false);
    expect(await api.startSteam()).toBe(true);
  });
});

describe('API - Desktop Mode IPC Bridge', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // Simulate Tauri v2 environment
    global.window = {
      __TAURI_INTERNALS__: {},
      location: { protocol: 'tauri:', hostname: 'tauri.localhost' },
    };
  });

  afterEach(() => {
    delete global.window;
  });

  it('detects isDesktop as true when Tauri internals exist', () => {
    expect(api.isDesktop()).toBe(true);
  });

  it('invokes get_state on getState', async () => {
    mockInvoke.mockResolvedValueOnce({ library: [{ id: 'portal-2' }] });
    const state = await api.getState();
    expect(mockInvoke).toHaveBeenCalledWith('get_state');
    expect(state.library[0].id).toBe('portal-2');
  });

  it('invokes get_runners on getRunners', async () => {
    mockInvoke.mockResolvedValueOnce([{ id: 'proton:ge', name: 'GE-Proton' }]);
    const runners = await api.getRunners();
    expect(mockInvoke).toHaveBeenCalledWith('get_runners');
    expect(runners[0].id).toBe('proton:ge');
  });

  it('invokes search_steam_games with query', async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await api.searchSteamGames('halflife');
    expect(mockInvoke).toHaveBeenCalledWith('search_steam_games', { query: 'halflife' });
  });

  it('invokes diagnose with id', async () => {
    mockInvoke.mockResolvedValueOnce({ checks: [] });
    await api.diagnoseGame('hl2');
    expect(mockInvoke).toHaveBeenCalledWith('diagnose', { id: 'hl2' });
  });

  it('invokes discover_exes with id', async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await api.discoverExecutables('hl2');
    expect(mockInvoke).toHaveBeenCalledWith('discover_exes', { id: 'hl2' });
  });

  it('invokes scan_game_setup with id and title', async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await api.scanSetup('prefix-1', 'Game Title');
    expect(mockInvoke).toHaveBeenCalledWith('scan_game_setup', {
      id: 'prefix-1',
      title: 'Game Title',
    });
  });

  it('invokes launch_game with extra arguments or null', async () => {
    mockInvoke.mockResolvedValueOnce({ status: 'launched', pid: 1234 });
    await api.launchGame('hl2', ['-fullscreen', '-novid']);
    expect(mockInvoke).toHaveBeenCalledWith('launch_game', {
      id: 'hl2',
      extraArgs: ['-fullscreen', '-novid'],
    });

    mockInvoke.mockResolvedValueOnce({ status: 'launched', pid: 1235 });
    await api.launchGame('hl2', []);
    expect(mockInvoke).toHaveBeenCalledWith('launch_game', {
      id: 'hl2',
      extraArgs: null,
    });
  });

  it('invokes stop_game with id', async () => {
    mockInvoke.mockResolvedValueOnce({ status: 'stopped', id: 'hl2' });
    await api.stopGame('hl2');
    expect(mockInvoke).toHaveBeenCalledWith('stop_game', { id: 'hl2' });
  });

  it('invokes run_action with payload', async () => {
    mockInvoke.mockResolvedValueOnce({ success: true });
    const payload = { action: 'game-import', id: 'my-game' };
    await api.runAction(payload);
    expect(mockInvoke).toHaveBeenCalledWith('run_action', { action: payload });
  });

  it('invokes window controls', async () => {
    await api.setWindowTranslucency('acrylic');
    expect(mockInvoke).toHaveBeenCalledWith('set_window_translucency', { effect: 'acrylic' });

    await api.windowMinimize();
    expect(mockInvoke).toHaveBeenCalledWith('window_minimize');

    await api.windowToggleMaximize();
    expect(mockInvoke).toHaveBeenCalledWith('window_toggle_maximize');

    await api.windowClose();
    expect(mockInvoke).toHaveBeenCalledWith('window_close');
  });

  it('invokes is_steam_running and start_steam in desktop mode', async () => {
    mockInvoke.mockResolvedValueOnce(true);
    const running = await api.isSteamRunning();
    expect(mockInvoke).toHaveBeenCalledWith('is_steam_running');
    expect(running).toBe(true);

    mockInvoke.mockResolvedValueOnce(true);
    const started = await api.startSteam();
    expect(mockInvoke).toHaveBeenCalledWith('start_steam');
    expect(started).toBe(true);
  });

  it('invokes storage and settings action helpers correctly', async () => {
    mockInvoke.mockResolvedValue({ success: true });

    await api.updateSettings({ theme_mode: 'amoled' });
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'settings-update', settings: { theme_mode: 'amoled' } },
    });

    await api.addStorageDrive('Fast SSD', '/mnt/nvme');
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'storage-add-drive', drive_label: 'Fast SSD', drive_path: '/mnt/nvme' },
    });

    await api.removeStorageDrive('drive-1');
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'storage-remove-drive', drive_id: 'drive-1' },
    });

    await api.setDefaultDrive('drive-2');
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'storage-set-default', drive_id: 'drive-2' },
    });

    await api.moveGameToDrive('portal-2', 'drive-2');
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'storage-move-game', id: 'portal-2', drive_id: 'drive-2' },
    });

    await api.scanInstalledGames();
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'scan-installed-games' },
    });

    await api.downloadProtonGE();
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'proton-download' },
    });

    await api.refreshPluginStore();
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'plugin-store-refresh' },
    });

    await api.installStorePlugin('steamidra');
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'plugin-store-install', id: 'steamidra' },
    });

    await api.syncSteamShortcuts();
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'steam-shortcuts-sync' },
    });

    await api.clearJob('job-123');
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'job-clear', id: 'job-123' },
    });

    await api.clearCompletedJobs();
    expect(mockInvoke).toHaveBeenCalledWith('run_action', {
      action: { action: 'job-clear-completed' },
    });
  });
});

describe('API - Busy State Tracking', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api._resetBusy();
    global.window = {
      __TAURI_INTERNALS__: {},
      location: { protocol: 'tauri:', hostname: 'tauri.localhost' },
    };
  });

  afterEach(() => {
    delete global.window;
    api._resetBusy();
  });

  it('notifies subscribers immediately of current busy state', () => {
    const events = [];
    const unsub = api.onBusyChange((busy) => events.push(busy));
    expect(events).toEqual([false]);
    expect(api.isBusy()).toBe(false);
    unsub();
  });

  it('transitions busy state during runAction', async () => {
    const events = [];
    const unsub = api.onBusyChange((busy) => events.push(busy));

    let resolveAction;
    mockInvoke.mockImplementationOnce(() => new Promise((resolve) => {
      resolveAction = resolve;
    }));

    const actionPromise = api.runAction({ action: 'steam-install' });
    expect(api.isBusy()).toBe(true);
    expect(events).toEqual([false, true]);

    resolveAction({ success: true });
    await actionPromise;

    expect(api.isBusy()).toBe(false);
    expect(events).toEqual([false, true, false]);
    unsub();
  });

  it('recovers busy state even if an action rejects with error', async () => {
    const events = [];
    const unsub = api.onBusyChange((busy) => events.push(busy));

    mockInvoke.mockRejectedValueOnce(new Error('Network failure'));

    await expect(api.runAction({ action: 'bad' })).rejects.toThrow('Network failure');

    expect(api.isBusy()).toBe(false);
    expect(events).toEqual([false, true, false]);
    unsub();
  });

  it('handles multiple concurrent operations correctly', async () => {
    const events = [];
    const unsub = api.onBusyChange((busy) => events.push(busy));

    let resolve1, resolve2;
    mockInvoke
      .mockImplementationOnce(() => new Promise((r) => { resolve1 = r; }))
      .mockImplementationOnce(() => new Promise((r) => { resolve2 = r; }));

    const p1 = api.searchSteamGames('portal');
    const p2 = api.runAction({ action: 'sync' });

    expect(api.isBusy()).toBe(true);
    // Was already notified true on the first request, stays true
    expect(events).toEqual([false, true, true]);

    resolve1([]);
    await p1;
    // p2 is still pending, so still busy
    expect(api.isBusy()).toBe(true);

    resolve2({ success: true });
    await p2;
    // Now both finished, so busy becomes false
    expect(api.isBusy()).toBe(false);
    expect(events[events.length - 1]).toBe(false);

    unsub();
  });
});
