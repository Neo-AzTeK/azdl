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
});
