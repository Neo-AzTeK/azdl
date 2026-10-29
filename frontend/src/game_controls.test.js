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
});
