import { describe, expect, it } from 'vitest';
import { reconcileData } from './state.js';

describe('reconcileData', () => {
  it('returns the previous reference when the incoming state is equal', () => {
    const previous = {
      library: [{ id: 'one', genres: ['Action'] }],
      settings: { theme: 'dark' },
    };

    expect(reconcileData(previous, {
      library: [{ id: 'one', genres: ['Action'] }],
      settings: { theme: 'dark' },
    })).toBe(previous);
  });

  it('copies changed branches while retaining unchanged siblings and records', () => {
    const unchangedGame = { id: 'one', title: 'One' };
    const changedGame = { id: 'two', is_running: false };
    const previous = {
      library: [unchangedGame, changedGame],
      settings: { theme: 'dark' },
    };

    const next = reconcileData(previous, {
      library: [
        { id: 'one', title: 'One' },
        { id: 'two', is_running: true },
      ],
      settings: { theme: 'dark' },
    });

    expect(next).not.toBe(previous);
    expect(next.library).not.toBe(previous.library);
    expect(next.library[0]).toBe(unchangedGame);
    expect(next.library[1]).not.toBe(changedGame);
    expect(next.settings).toBe(previous.settings);
    expect(next.library[1].is_running).toBe(true);
  });

  it('preserves removals and array length changes', () => {
    const previous = { tasks: [{ id: 'one' }, { id: 'two' }], obsolete: true };
    const next = reconcileData(previous, { tasks: [{ id: 'one' }] });

    expect(next.tasks).toHaveLength(1);
    expect(next).not.toHaveProperty('obsolete');
  });

  it('copies special JSON keys as data properties', () => {
    const previous = {};
    const next = JSON.parse('{"__proto__":{"safe":true}}');
    const reconciled = reconcileData(previous, next);

    expect(Object.hasOwn(reconciled, '__proto__')).toBe(true);
    expect(Object.getPrototypeOf(reconciled)).toBe(Object.prototype);
    expect(reconciled.__proto__).toEqual({ safe: true });
  });
});
