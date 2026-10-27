import { describe, expect, it } from 'vitest';
import { reconcileData, createRefreshQueue, progressTopology, applyProgressSnapshot } from './state.js';

describe('lightweight progress', () => {
  it('updates transfers without replacing library or settings, and ignores bytes for full-refresh decisions', () => {
    const initial = { library: [{ id: 'game' }], settings: { theme: 'dark' }, jobs: [{ id: 'download', status: 'Downloading', bytes: 1 }], tasks: [], running_games: {} };
    const progress = { jobs: [{ id: 'download', status: 'Downloading', bytes: 20 }], tasks: [], running_games: {} };
    const next = applyProgressSnapshot(initial, progress);
    expect(next.library).toBe(initial.library);
    expect(next.settings).toBe(initial.settings);
    expect(next.jobs[0].bytes).toBe(20);
    expect(progressTopology(next)).toBe(progressTopology(initial));
    expect(progressTopology({ ...next, jobs: [{ ...next.jobs[0], status: 'Complete' }] })).not.toBe(progressTopology(initial));
  });
});

describe('refresh queue', () => {
  it('shares completion across callers and keeps one trailing refresh for events during a request', async () => {
    const releases = [];
    let count = 0;
    const refresh = createRefreshQueue(() => {
      count++;
      return new Promise((resolve) => releases.push(resolve));
    });
    const first = refresh();
    expect(refresh()).toBe(first);
    await Promise.resolve();
    expect(count).toBe(1);
    expect(refresh()).toBe(first);
    expect(refresh()).toBe(first);
    releases.shift()();
    await Promise.resolve();
    expect(count).toBe(2);
    releases.shift()();
    await first;
    const next = refresh();
    await Promise.resolve();
    expect(count).toBe(3);
    releases.shift()();
    await next;
  });

  it('can refresh again after a failed request', async () => {
    let attempts = 0;
    const refresh = createRefreshQueue(async () => {
      if (++attempts === 1) throw new Error('offline');
    });
    await expect(refresh()).rejects.toThrow('offline');
    await refresh();
    expect(attempts).toBe(2);
  });
});

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
