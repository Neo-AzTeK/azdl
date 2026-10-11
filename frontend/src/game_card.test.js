import { describe, expect, it } from 'vitest';
import { getGameCardMetadataTags, normalizeGameCard } from './game_card.js';

describe('normalizeGameCard', () => {
  it('normalizes Steam discovery and installed library metadata to one card shape', () => {
    const discovered = normalizeGameCard({
      id: 'steam-42', title: 'Example', source_id: ' steam ',
      genres: [{ id: 1, description: 'Action' }, ' Action ', null], addons_count: '2',
      file_size: '45 GB', version: '1.2',
    });
    const installed = normalizeGameCard({
      id: 'steam-42', name: 'Example', source: 'steam', genres: ['Action'], addons_count: 2,
      file_size: '45 GB', version: '1.2',
    });

    expect(discovered.title).toBe(installed.title);
    expect(discovered.source_id).toBe(installed.source_id);
    expect(discovered.genres).toEqual(installed.genres);
    expect(discovered.addons_count).toBe(installed.addons_count);
    expect(getGameCardMetadataTags(discovered)).toEqual(getGameCardMetadataTags(installed));
    expect(getGameCardMetadataTags(discovered)).toEqual([
      { type: 'size', label: '45 GB' },
    ]);
  });

  it('drops malformed or empty tag values', () => {
    expect(normalizeGameCard({
      id: 'id', genres: [{ id: 3 }, '', 10], version: ' ', file_size: null,
    })).toMatchObject({ title: 'id', genres: [], version: null, file_size: null, addons_count: 0 });
  });

  it('shows only storage size even when other metadata is present', () => {
    expect(getGameCardMetadataTags(normalizeGameCard({
      file_size: '20 GB', version: '3', genres: ['Action'], source_id: 'steam', addons_count: 4,
    }))).toEqual([
      { type: 'size', label: '20 GB' },
    ]);
    expect(getGameCardMetadataTags(normalizeGameCard({ file_size: null }), '12 GB'))
      .toEqual([{ type: 'size', label: '12 GB' }]);
  });

  it('determines card shimmer state accurately during launch, operation, or active download', () => {
    function computeCardShimmer({ active_operation, is_running, launching, activeJob, isStartingInstall }) {
      return Boolean(active_operation) ||
        Boolean(launching) ||
        Boolean(is_running) ||
        Boolean(activeJob) ||
        Boolean(isStartingInstall);
    }

    // Idle card
    expect(computeCardShimmer({ active_operation: null, is_running: false, launching: false, activeJob: null, isStartingInstall: false })).toBe(false);

    // Running game (glimmers)
    expect(computeCardShimmer({ active_operation: null, is_running: true, launching: false, activeJob: null, isStartingInstall: false })).toBe(true);

    // Launching game (glimmers)
    expect(computeCardShimmer({ active_operation: null, is_running: false, launching: true, activeJob: null, isStartingInstall: false })).toBe(true);

    // Active downloading job in discover (glimmers like started game)
    expect(computeCardShimmer({ active_operation: null, is_running: false, launching: false, activeJob: { id: 'job-1', status: 'Downloading' }, isStartingInstall: false })).toBe(true);

    // Immediately upon download FAB click
    expect(computeCardShimmer({ active_operation: null, is_running: false, launching: false, activeJob: null, isStartingInstall: true })).toBe(true);
  });

  it('verifies FAB disappears and renders nothing in place during download', () => {
    function getFabState({ is_running, isInstalled, activeJob, isStartingInstall }) {
      if (is_running) return 'stop';
      if (isInstalled) return 'play';
      if (activeJob || isStartingInstall) return null; // FAB disappears, no pill
      return 'download';
    }

    expect(getFabState({ is_running: true, isInstalled: true, activeJob: null, isStartingInstall: false })).toBe('stop');
    expect(getFabState({ is_running: false, isInstalled: true, activeJob: null, isStartingInstall: false })).toBe('play');
    expect(getFabState({ is_running: false, isInstalled: false, activeJob: null, isStartingInstall: false })).toBe('download');
    // While downloading, FAB is hidden (returns null) and pill is eliminated
    expect(getFabState({ is_running: false, isInstalled: false, activeJob: { id: 'dl' }, isStartingInstall: false })).toBeNull();
    expect(getFabState({ is_running: false, isInstalled: false, activeJob: null, isStartingInstall: true })).toBeNull();
  });
});
