import { writable } from 'svelte/store';

export const notifications = writable([]);

let nextId = 1;

/**
 * Trigger a unified notification toast in the UI.
 * @param {string} message - Notification text
 * @param {'info' | 'success' | 'error' | 'warning'} type - Semantic notification level
 * @param {number} duration - Auto-dismiss timeout in ms (default 4000ms, 0 for persistent)
 * @returns {number} Notification ID
 */
export function notify(message, type = 'info', duration = 4000) {
  const id = nextId++;
  const item = { id, message, type, duration };

  notifications.update((list) => [...list, item]);

  if (duration > 0) {
    setTimeout(() => {
      dismissNotification(id);
    }, duration);
  }

  return id;
}

notify.info = (msg, duration) => notify(msg, 'info', duration);
notify.success = (msg, duration) => notify(msg, 'success', duration);
notify.error = (msg, duration) => notify(msg, 'error', duration);
notify.warning = (msg, duration) => notify(msg, 'warning', duration);

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
