import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';

// Desktop Webview Quirk Guards (Zoom lock, trackpad gestures, drag-drop navigation, shortcuts)
const zoomKeys = new Set(['+', '=', '-', '_', '0', ')', '°', ']']);
const zoomCodes = new Set([
  'Equal',
  'Minus',
  'Digit0',
  'Digit6',
  'NumpadAdd',
  'NumpadSubtract',
  'Numpad0',
]);

function preventWebviewShortcuts(event) {
  // 1. Wheel zoom (Ctrl + Mouse wheel or touchpad pinch)
  if (event.type === 'wheel') {
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
    }
    return;
  }

  // 2. Keyboard zoom (Ctrl/Cmd + Plus/Minus/0 on any layout)
  if (event.ctrlKey || event.metaKey) {
    if (zoomKeys.has(event.key) || zoomCodes.has(event.code)) {
      event.preventDefault();
      return;
    }
    // Block accidental webview reload in production
    if (import.meta.env.PROD && (event.key === 'r' || event.key === 'R')) {
      event.preventDefault();
      return;
    }
  }

  // 3. Block F5 reload in production
  if (import.meta.env.PROD && event.key === 'F5') {
    event.preventDefault();
  }
}

// Intercept keyboard and wheel zoom
window.addEventListener('keydown', preventWebviewShortcuts, { capture: true });
window.addEventListener('wheel', preventWebviewShortcuts, { capture: true, passive: false });

// Block WebKit trackpad multi-finger pinch gestures (WebKitGTK & Safari)
window.addEventListener('gesturestart', (e) => e.preventDefault(), { capture: true });
window.addEventListener('gesturechange', (e) => e.preventDefault(), { capture: true });
window.addEventListener('gestureend', (e) => e.preventDefault(), { capture: true });

// Prevent accidental file/image drop that navigates webview away from launcher
window.addEventListener('dragover', (e) => e.preventDefault(), { capture: true });
window.addEventListener('drop', (e) => e.preventDefault(), { capture: true });

// Prevent browser context menu in production (except on text inputs for copy/paste)
window.addEventListener('contextmenu', (e) => {
  const target = e.target;
  const isInput = target && (
    target.tagName === 'INPUT' ||
    target.tagName === 'TEXTAREA' ||
    target.isContentEditable
  );
  if (!isInput && import.meta.env.PROD) {
    e.preventDefault();
  }
}, { capture: true });

const app = mount(App, {
  target: document.getElementById('app'),
});

export default app;
