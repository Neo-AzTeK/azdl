import { mount, tick } from 'svelte';
import '../src/app.css';
import { applyTheme } from '../src/theme.js';

const snapshots = Object.fromEntries(['alpha', 'beta'].map((id) => [id, {
  overlay: { enabled: false, can_toggle: true, mixed: false },
  steamstub: { applied: false, stub_present: true },
  patch: 'disabled', ea_heal_applied: false, ea_heal_available: true,
  dlc_patch_applied: false, dlc_patch_available: true,
  goldberg_has_settings: false, goldberg_compatible: true,
  runtime: { available: true, managed: true, prefix: `/fixture/${id}/pfx`, status: {vcredist_x64:false,directx:false} },
}]));
let releaseCommand;
let rejectCommand;
let calls = [];
window.__TAURI_INTERNALS__ = {
  invoke: async (command, args) => {
    if (command === 'get_game_controls') return structuredClone(snapshots[args.id]);
    if (command === 'run_action') {
      calls.push(args.action);
      return new Promise((resolve, reject) => { releaseCommand = resolve; rejectCommand = reject; });
    }
    if (command === 'get_game_dlc') return { available: [], installed: [] };
    if (command === 'diagnose') return { checks: [] };
    return null;
  },
};
const results = [];
function check(value, label) { if (!value) throw new Error(label); results.push(label); }
async function until(predicate) {
  for (let attempt = 0; attempt < 100; attempt++) {
    await tick();
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error('Timed out waiting for UI state');
}
function selectTab(value) { document.querySelector(`input[type="radio"][value="${value}"]`).click(); }
const switches = () => [...document.querySelectorAll('#section-patches input[role="switch"]')];

try {
  applyTheme('#10B981', 'dark');
  const { default: Harness } = await import('./GameControlsHarness.svelte');
  const harness = mount(Harness, { target: document.getElementById('app') });
  await until(() => document.querySelector('input[value="patches"]'));
  selectTab('patches');
  await until(() => switches().length === 6 && !switches()[0].disabled);
  check(!document.querySelector('#section-patches button'), 'Patch cards expose switches only');
  check(!document.querySelector('#section-patches').textContent.includes('Runtime Dependencies'), 'Runtime dependencies are outside Patches');
  check(!switches()[0].checked && !switches()[0].disabled, 'Disabled Windows overlay remains available to enable');
  switches()[0].click();
  await until(() => calls.length === 1);
  check(switches().every((input) => input.disabled), 'All switches lock while a command runs');
  check(!switches()[0].checked, 'Pending switch retains its last confirmed state');
  snapshots.alpha.overlay.enabled = true;
  releaseCommand({ success: true });
  await until(() => switches()[0].checked && !switches()[0].disabled);
  check(calls.length === 1, 'A toggle sends exactly one command');
  switches()[0].click();
  await until(() => calls.length === 2);
  rejectCommand(new Error('Fixture failure'));
  await until(() => !switches()[0].disabled);
  check(switches()[0].checked, 'Failed command restores observed state');
  const input = switches()[0];
  input.dispatchEvent(new KeyboardEvent('keydown', {code:'ArrowLeft',bubbles:true}));
  await until(() => calls.length === 3);
  snapshots.alpha.overlay.enabled = false;
  releaseCommand({ success: true });
  await until(() => !switches()[0].checked && !switches()[0].disabled);
  check(true, 'Keyboard switch actions confirm through read-back');
  const drag = switches()[0].closest('.m3-container');
  drag.dispatchEvent(new PointerEvent('pointerdown', {clientX:100,bubbles:true}));
  window.dispatchEvent(new PointerEvent('pointerup', {clientX:140,bubbles:true}));
  await until(() => calls.length === 4);
  snapshots.alpha.overlay.enabled = true;
  releaseCommand({ success: true });
  await until(() => switches()[0].checked && !switches()[0].disabled);
  check(true, 'Dragging the switch sends and confirms one command');
  selectTab('config');
  await until(() => document.body.textContent.includes('Runtime Dependencies') && document.body.textContent.includes('Install VC++'));
  check(!document.getElementById('section-patches'), 'Runtime section lives in Launch & Runners');
  selectTab('patches');
  await until(() => switches().length === 6 && !switches()[0].disabled);
  switches()[0].click();
  await until(() => calls.length === 5);
  harness.selectGame('beta');
  await until(() => document.querySelector('.game-page-title')?.textContent === 'beta' || document.body.textContent.includes('beta'));
  snapshots.alpha.overlay.enabled = false;
  releaseCommand({ success: true });
  await until(() => switches().length === 6 && !switches()[0].disabled);
  check(!switches()[0].checked, 'Completing a previous game command cannot change the selected game state');
  document.getElementById('result').textContent = `PASS: ${results.length} checks\n${results.join('\n')}`;
  document.getElementById('result').dataset.result = 'pass';
} catch (error) {
  document.getElementById('result').textContent = `FAIL: ${error.stack}`;
  document.getElementById('result').dataset.result = 'fail';
  console.error(error);
}
