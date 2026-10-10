import { beforeEach, describe, expect, it, vi } from 'vitest';
import { resolveGameArtwork } from './api.js';
import { fetchArtwork, getResolvedArtwork, setResolvedArtwork, subscribeArtwork } from './covers.js';

vi.mock('./api.js', () => ({ resolveGameArtwork: vi.fn() }));

describe('artwork subscriptions', () => {
  beforeEach(() => resolveGameArtwork.mockReset());

  it('notifies only listeners registered for the changed artwork key', () => {
    const received = [];
    const unsubscribe = subscribeArtwork((key) => received.push(key), 'Game One');

    setResolvedArtwork('Game Two', { banner_url: 'two.jpg' });
    expect(received).toEqual([]);

    setResolvedArtwork('game one', { banner_url: 'one.jpg' });
    expect(received).toEqual(['game one']);

    unsubscribe();
    setResolvedArtwork('game one', { banner_url: 'updated.jpg' });
    expect(received).toEqual(['game one']);
  });

  it('supports a listener subscribed to several artwork keys', () => {
    const received = [];
    const unsubscribe = subscribeArtwork((key) => received.push(key), ['Game One', 'Game Two']);

    setResolvedArtwork('game two', { banner_url: 'two.jpg' });
    setResolvedArtwork('game three', { banner_url: 'three.jpg' });
    expect(received).toEqual(['game two']);

    unsubscribe();
  });

  it('bounds the in-memory artwork cache', () => {
    for (let index = 0; index <= 256; index += 1) {
      setResolvedArtwork(`cache-limit-${index}`, { banner_url: `${index}.jpg` });
    }

    expect(getResolvedArtwork('cache-limit-0')).toBeNull();
    expect(getResolvedArtwork('cache-limit-256')).toEqual({ banner_url: '256.jpg' });
  });

  it('bounds persisted artwork entries and removes legacy overflow', () => {
    const values = new Map();
    const storage = {
      get length() { return values.size; },
      getItem(key) { return values.get(key) ?? null; },
      setItem(key, value) { values.set(key, String(value)); },
      removeItem(key) { values.delete(key); },
      key(index) { return [...values.keys()][index] ?? null; },
    };
    const previousWindow = globalThis.window;
    globalThis.window = { localStorage: storage };

    try {
      for (let index = 0; index <= 256; index += 1) {
        setResolvedArtwork(`persistent-limit-${index}`, { banner_url: `${index}.jpg` });
      }

      const artworkEntries = [...values.keys()].filter((key) => key.startsWith('azdl_art_') && key !== 'azdl_art_index_v1');
      expect(artworkEntries).toHaveLength(256);
      expect(values.has('azdl_art_persistent-limit-0')).toBe(false);
      expect(values.has('azdl_art_persistent-limit-256')).toBe(true);
    } finally {
      if (previousWindow === undefined) delete globalThis.window;
      else globalThis.window = previousWindow;
    }
  });

  it('limits concurrent artwork resolver requests while resolving every query', async () => {
    let activeRequests = 0;
    let maximumActiveRequests = 0;
    resolveGameArtwork.mockImplementation(async (query) => {
      activeRequests += 1;
      maximumActiveRequests = Math.max(maximumActiveRequests, activeRequests);
      await new Promise((resolve) => setTimeout(resolve, 5));
      activeRequests -= 1;
      return { banner_url: `${query}.jpg` };
    });

    const results = await Promise.all(
      Array.from({ length: 24 }, (_, index) => fetchArtwork(`limited-${index}`))
    );

    expect(maximumActiveRequests).toBe(8);
    expect(results).toHaveLength(24);
    expect(results.every((entry) => entry?.banner_url)).toBe(true);
  });
});
