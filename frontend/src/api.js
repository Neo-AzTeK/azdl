// In-Process Tauri v2 IPC Client for azdl (Zero HTTP overhead)
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

export function isDesktop() {
  if (typeof window === 'undefined') return false;
  try {
    return Boolean(
      window.__TAURI_INTERNALS__ ||
      window.isTauri ||
      (window.location && (window.location.protocol === 'tauri:' || window.location.hostname === 'tauri.localhost')) ||
      isTauri()
    );
  } catch {
    return false;
  }
}

export function onStateChange(callback) {
  return onBackendEvent('state-changed', callback);
}

export function onProgressChange(callback) {
  return onBackendEvent('progress-changed', callback);
}

function onBackendEvent(name, callback) {
  if (!isDesktop()) {
    return () => {};
  }
  let unlistenFn = null;
  let active = true;

  listen(name, (event) => {
    try {
      callback(event);
    } catch (err) {
      console.error('Error in state-changed listener:', err);
    }
  })
    .then((unlisten) => {
      if (active) {
        unlistenFn = unlisten;
      } else if (typeof unlisten === 'function') {
        unlisten();
      }
    })
    .catch((err) => {
      console.warn('Failed to listen for state-changed event:', err);
    });

  return () => {
    active = false;
    if (unlistenFn) {
      try {
        unlistenFn();
      } catch (err) {
        console.warn('Error during unlisten for state-changed:', err);
      }
      unlistenFn = null;
    }
  };
}

export async function getProgress() {
  if (isDesktop()) return await invoke('get_progress');
  return { jobs: [], tasks: [], running_games: {} };
}

// Busy state tracker for global UI indicator
let activeRequests = 0;
const busyListeners = new Set();

export function onBusyChange(fn) {
  busyListeners.add(fn);
  try {
    fn(activeRequests > 0);
  } catch (e) {
    console.error('Error in busy listener:', e);
  }
  return () => busyListeners.delete(fn);
}

export function isBusy() {
  return activeRequests > 0;
}

export function _resetBusy() {
  activeRequests = 0;
  busyListeners.clear();
}

function notifyBusy() {
  const busy = activeRequests > 0;
  for (const fn of busyListeners) {
    try {
      fn(busy);
    } catch (e) {
      console.error('Error in busy listener:', e);
    }
  }
}

async function withBusy(fn) {
  activeRequests++;
  notifyBusy();
  try {
    return await fn();
  } finally {
    activeRequests = Math.max(0, activeRequests - 1);
    notifyBusy();
  }
}

export async function getState() {
  if (isDesktop()) {
    return await invoke('get_state');
  }
  // Fallback for browser preview / development
  return {
    library: [],
    jobs: [],
    running_games: {},
    tasks: [],
    pending: [],
    settings: {
      storage_drives: [],
      default_drive: null,
      default_runner: 'auto',
      global_arguments: [],
      steamstub_auto_unpack: true,
      onlinefix_overlay_enabled: true,
      theme_mode: 'system',
      theme_seed_color: '#3f51b5',
      window_effect: 'none',
    },
    diagnostics: [],
    data_dir: '/preview/azdl',
    platform: 'linux',
  };
}

export async function getGameDlc(id, candidateDlcId = null) {
  if (!isDesktop()) return { available: [], installed: [] };
  return await withBusy(() => invoke('get_game_dlc', { id, candidateDlcId }));
}

export async function getRunners() {
  if (isDesktop()) {
    return await invoke('get_runners');
  }
  return [
    { id: 'auto', name: 'Auto (Recommended)', runner_type: 'auto' },
    { id: 'native', name: 'Native', runner_type: 'native' },
    { id: 'wine', name: 'System Wine', runner_type: 'wine' },
  ];
}

export async function getSteamDetails(appId) {
  if (!appId) return null;
  const numId = parseInt(appId, 10);
  if (isNaN(numId) || numId <= 0) return null;
  try {
    if (isDesktop()) {
      return await invoke('get_steam_details', { appId: numId });
    }
  } catch (e) {
    console.warn(`Failed to fetch Steam details for ${numId}:`, e);
  }
  return null;
}

export async function searchSteamGames(query) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('search_steam_games', { query });
    }
    return [];
  });
}

export async function diagnoseGame(id) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('diagnose', { id });
    }
    return { checks: [] };
  });
}

export async function discoverExecutables(id) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('discover_exes', { id });
    }
    return [];
  });
}

export async function scanSetup(id, title = null) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('scan_game_setup', { id, title });
    }
    return [];
  });
}

export async function launchGame(id, extraArgs = []) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('launch_game', { id, extraArgs: extraArgs.length ? extraArgs : null });
    }
    return { status: 'mock_launched', id };
  });
}

export async function stopGame(id) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('stop_game', { id });
    }
    return { status: 'mock_stopped', id };
  });
}

export async function runAction(payload) {
  return await withBusy(async () => {
    if (isDesktop()) {
      return await invoke('run_action', { action: payload });
    }
    return { success: true };
  });
}

export async function windowMinimize() {
  if (isDesktop()) {
    return await invoke('window_minimize');
  }
}

export async function windowToggleMaximize() {
  if (isDesktop()) {
    return await invoke('window_toggle_maximize');
  }
}

export async function windowClose() {
  if (isDesktop()) {
    return await invoke('window_close');
  }
}

export async function updateSettings(settings) {
  return await runAction({ action: 'settings-update', settings });
}

export async function addStorageDrive(label, drive_path) {
  return await runAction({ action: 'storage-add-drive', drive_label: label, drive_path });
}

export async function removeStorageDrive(drive_id) {
  return await runAction({ action: 'storage-remove-drive', drive_id });
}

export async function setDefaultDrive(drive_id) {
  return await runAction({ action: 'storage-set-default', drive_id });
}

export async function moveGameToDrive(gameId, drive_id) {
  return await runAction({ action: 'storage-move-game', id: gameId, drive_id });
}

export async function scanInstalledGames() {
  return await runAction({ action: 'scan-installed-games' });
}


export async function downloadProtonGE() {
  return await runAction({ action: 'proton-download' });
}

export async function isSteamRunning() {
  if (isDesktop()) {
    return await invoke('is_steam_running');
  }
  return false;
}

export async function syncSteamShortcuts() {
  return await runAction({ action: 'steam-shortcuts-sync' });
}

export async function clearJob(id) {
  return await runAction({ action: 'job-clear', id });
}

export async function clearCompletedJobs() {
  return await runAction({ action: 'job-clear-completed' });
}

export async function pauseJob(id) {
  return await runAction({ action: 'job-pause', id });
}

export async function resumeJob(id) {
  return await runAction({ action: 'job-resume', id });
}

export async function cancelJob(id) {
  return await runAction({ action: 'job-cancel', id });
}

export async function openGameFolder(gameIdOrPath) {
  if (gameIdOrPath && (gameIdOrPath.startsWith('/') || gameIdOrPath.includes(':\\') || gameIdOrPath.includes(':/'))) {
    return await runAction({ action: 'open-folder', location: gameIdOrPath });
  }
  return await runAction({ action: 'open-folder', id: gameIdOrPath });
}

export async function resolveGameArtwork(query) {
  if (!query) return null;
  if (isDesktop()) {
    try {
      return await invoke('resolve_game_artwork', { query });
    } catch (e) {
      console.warn('Failed to resolve game artwork via Tauri command:', e);
      return null;
    }
  }
  return null;
}

/**
 * Normalize and parse structured launcher errors returned from backend IPC or HTTP.
 * @param {any} err
 * @returns {{ code: string, message: string, [key: string]: any }}
 */
export function parseLauncherError(err) {
  if (!err) return { code: 'general', message: 'An unknown error occurred.' };

  if (typeof err === 'object') {
    return {
      code: err.code || 'general',
      message: err.message || JSON.stringify(err),
      ...err,
    };
  }

  if (typeof err === 'string') {
    try {
      const parsed = JSON.parse(err);
      if (parsed && typeof parsed === 'object') {
        return {
          code: parsed.code || 'general',
          message: parsed.message || err,
          ...parsed,
        };
      }
    } catch {}
    return { code: 'general', message: err };
  }

  return { code: 'general', message: String(err) };
}
