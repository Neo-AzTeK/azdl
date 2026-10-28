import { describe, expect, it } from 'vitest';
import { controlActionConfirmed } from './game_controls.js';

describe('observed control outcomes', () => {
  it('uses actual overlay state, including a disabled overlay that still exists', () => {
    const before = { overlay: { enabled: false, can_toggle: true } };
    expect(controlActionConfirmed('overlay-toggle', before, { overlay: { enabled: true } })).toBe(true);
    expect(controlActionConfirmed('overlay-toggle', before, before)).toBe(false);
  });
  it('does not report a successful install when a runtime remains missing', () => {
    expect(controlActionConfirmed('install-directx', {}, { runtime: { status: { directx: false } } })).toBe(false);
    expect(controlActionConfirmed('install-vcredist', {}, { runtime: { status: { vcredist_x64: true } } })).toBe(true);
  });
  it('confirms rollback and rejects missing or unchanged observations', () => {
    expect(controlActionConfirmed('steamstub-rollback', {}, { steamstub: { applied: false } })).toBe(true);
    expect(controlActionConfirmed('ea-heal', {}, { ea_heal_applied: false })).toBe(false);
    expect(controlActionConfirmed('goldberg-sync', {}, {})).toBe(false);
    expect(controlActionConfirmed('patch-toggle', { patch: 'enabled' }, { patch: 'disabled' })).toBe(true);
  });
});
