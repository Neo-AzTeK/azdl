import { writable } from 'svelte/store';
import { parseLauncherError } from './api.js';

export const notifications = writable([]);

let nextId = 1;

/**
 * Trigger a unified notification toast in the UI.
 * @param {string} message - Notification text
 * @param {'info' | 'success' | 'error' | 'warning'} type - Semantic notification level
 * @param {number} duration - Auto-dismiss timeout in ms (default 4000ms, 0 for persistent)
 * @param {{ label: string, onClick: () => void } | null} action - Optional actionable button
 * @returns {number} Notification ID
 */
export function notify(message, type = 'info', duration = 4000, action = null) {
  const id = nextId++;
  const item = { id, message, type, duration, action };

  notifications.update((list) => [...list, item]);

  if (duration > 0) {
    setTimeout(() => {
      dismissNotification(id);
    }, duration);
  }

  return id;
}

notify.info = (msg, duration, action) => notify(msg, 'info', duration, action);
notify.success = (msg, duration, action) => notify(msg, 'success', duration, action);
notify.error = (msg, duration, action) => notify(msg, 'error', duration, action);
notify.warning = (msg, duration, action) => notify(msg, 'warning', duration, action);

/**
 * Display a structured actionable error toast with a contextual remediation button.
 * @param {any} err - Structured or string error from backend IPC or actions
 * @param {{
 *   onOpenSettings?: (category?: string) => void,
 *   onConfigureExecutable?: () => void,
 * }} actionHandlers - Contextual callback handlers
 * @returns {number} Notification ID
 */
export function notifyError(err, actionHandlers = {}) {
  const parsed = parseLauncherError(err);
  let action = null;

  switch (parsed.code) {
    case 'runner_not_found':
      if (actionHandlers.onOpenSettings) {
        action = {
          label: 'Runners Settings',
          onClick: () => actionHandlers.onOpenSettings('runners'),
        };
      }
      break;
    case 'no_plugin_available':
    case 'plugin_not_found':
      if (actionHandlers.onOpenSettings) {
        action = {
          label: 'Plugins Store',
          onClick: () => actionHandlers.onOpenSettings('plugins'),
        };
      }
      break;
    case 'drive_full':
    case 'drive_not_found':
      if (actionHandlers.onOpenSettings) {
        action = {
          label: 'Manage Storage',
          onClick: () => actionHandlers.onOpenSettings('storage'),
        };
      }
      break;
    case 'executable_not_found':
      if (actionHandlers.onConfigureExecutable) {
        action = {
          label: 'Locate Executable',
          onClick: actionHandlers.onConfigureExecutable,
        };
      }
      break;
    default:
      break;
  }

  return notify(parsed.message, 'error', action ? 7000 : 4000, action);
}

/**
 * Dismiss a notification by ID.
 * @param {number} id
 */
export function dismissNotification(id) {
  notifications.update((list) => list.filter((n) => n.id !== id));
}

/**
 * Clear all currently active notifications.
 */
export function clearNotifications() {
  notifications.set([]);
}
