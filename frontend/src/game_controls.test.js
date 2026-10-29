import { describe, expect, it } from 'vitest';
import { controlActionConfirmed } from './game_controls.js';

describe('observed control outcomes', () => {
  it('does not report a successful install when a runtime remains missing', () => {
    expect(controlActionConfirmed('install-directx', { runtime: { status: { directx: false } } })).toBe(false);
    expect(controlActionConfirmed('install-vcredist', { runtime: { status: { vcredist_x64: true } } })).toBe(true);
  });
  it('rejects missing observations and passes actions without a read-back', () => {
    expect(controlActionConfirmed('install-directx', {})).toBe(false);
    expect(controlActionConfirmed('args-set', {})).toBe(true);
  });
  it('confirms patch and fix actions from the refetched game state', () => {
    expect(controlActionConfirmed('steamstub-unpack', { steamstub: { applied: true } })).toBe(true);
    expect(controlActionConfirmed('steamstub-unpack', { steamstub: { applied: false } })).toBe(false);
    expect(controlActionConfirmed('steamstub-rollback', { steamstub: { applied: false } })).toBe(true);
    expect(controlActionConfirmed('dlc-license-patch', { dlc_patch_applied: true })).toBe(true);
    expect(controlActionConfirmed('dlc-license-rollback', { dlc_patch_applied: true })).toBe(false);
    expect(controlActionConfirmed('goldberg-sync', { goldberg_has_settings: true })).toBe(true);
    expect(controlActionConfirmed('ea-heal', { ea_heal_applied: true })).toBe(true);
    expect(controlActionConfirmed('ea-rollback', { ea_heal_applied: false })).toBe(true);
  });
});
