import { describe, expect, it } from 'vitest';
import { reconcileData, createRefreshQueue, createRefreshCoordinator, progressTopology, applyProgressSnapshot } from './state.js';

describe('full and progress refresh coordination', () => {
  it('gives a queued full request priority over redundant progress work', async () => {
    const calls = [];
    const coordinator = createRefreshCoordinator(async () => calls.push('full'), async () => calls.push('progress'));
    await Promise.all([coordinator.refreshProgress(), coordinator.refreshFull(), coordinator.refreshProgress()]);
    expect(calls).toEqual(['full']);
  });

  it('waits for an existing progress request before fetching full state', async () => {
    let releaseProgress;
    let startedProgress;
    const started = new Promise((resolve) => startedProgress = resolve);
    const calls = [];
    const coordinator = createRefreshCoordinator(async () => calls.push('full'), async () => {
      calls.push('progress-start');
      startedProgress();
      await new Promise((resolve) => releaseProgress = resolve);
      calls.push('progress-end');
    });
    const progress = coordinator.refreshProgress();
    await started;
    const full = coordinator.refreshFull();
    await Promise.resolve();
    expect(calls).toEqual(['progress-start']);
    releaseProgress();
    await Promise.all([progress, full]);
    expect(calls).toEqual(['progress-start', 'progress-end', 'full']);
  });

  it('allows fallback progress again after a failed full refresh', async () => {
    let progressCalls = 0;
    const coordinator = createRefreshCoordinator(async () => { throw new Error('unavailable'); }, async () => progressCalls++);
    await expect(coordinator.refreshFull()).rejects.toThrow('unavailable');
    await coordinator.refreshProgress();
    expect(progressCalls).toBe(1);
  });

  it('suppresses redundant progress throughout a full-refresh burst, then resumes it', async () => {
    let firstStarted;
    let secondStarted;
    const firstStart = new Promise((resolve) => firstStarted = resolve);
    const secondStart = new Promise((resolve) => secondStarted = resolve);
    const releases = [];
    let fullCalls = 0;
    let progressCalls = 0;
    const coordinator = createRefreshCoordinator(() => new Promise((resolve) => {
      releases.push(resolve);
      if (++fullCalls === 1) firstStarted();
      else secondStarted();
    }), async () => progressCalls++);
    const first = coordinator.refreshFull();
    await firstStart;
    const second = coordinator.refreshFull();
    await coordinator.refreshProgress();
    expect(progressCalls).toBe(0);
    releases.shift()();
    await secondStart;
    await coordinator.refreshProgress();
    expect(progressCalls).toBe(0);
    releases.shift()();
    await Promise.all([first, second]);
    await coordinator.refreshProgress();
    expect(progressCalls).toBe(1);
    expect(fullCalls).toBe(2);
  });
});

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
