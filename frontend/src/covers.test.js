import { beforeEach, describe, expect, it, vi } from 'vitest';
import { resolveGameArtwork } from './api.js';
import {
  fetchArtwork,
  getActiveDisplayThumbnail,
  getActiveThumbnailPair,
  getCandidateThumbnails,
  getResolvedArtwork,
  getThumbnailCandidates,
  setResolvedArtwork,
  subscribeArtwork,
} from './covers.js';

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

describe('concurrent thumbnail loading (fallback and HQ)', () => {
  it('separates high quality candidates and fallback candidates for a game with steam_app_id', () => {
    const game = {
      id: 'steam-1091500',
      steam_app_id: 1091500,
      title: 'Cyberpunk 2077',
    };
    const { hq, fallback } = getThumbnailCandidates(game);

    expect(hq).toContain('https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1091500/header.jpg');
    expect(hq).toContain('https://cdn.cloudflare.steamstatic.com/steam/apps/1091500/capsule_616x353.jpg');

    expect(fallback).toContain('https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1091500/capsule_231x87.jpg');
    expect(fallback).toContain('https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1091500/capsule_184x69.jpg');
    expect(fallback).toContain('https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1091500/library_600x900.jpg');

    // Ensure no overlapping URLs between HQ and fallback
    for (const url of hq) {
      expect(fallback).not.toContain(url);
    }

    // Ensure getCandidateThumbnails preserves ordered combination
    const combined = getCandidateThumbnails(game);
    expect(combined).toEqual([...hq, ...fallback]);
  });

  it('preserves catalog explicit images across HQ and fallback', () => {
    const game = {
      id: 'portal-2',
      title: 'Portal 2',
      header_image: 'https://cdn.example.com/p2-header.jpg',
      small_capsule_image: 'https://cdn.example.com/p2-small.jpg',
      cover_url: 'https://cdn.example.com/p2-cover.jpg',
    };
    const { hq, fallback } = getThumbnailCandidates(game);

    expect(hq[0]).toBe('https://cdn.example.com/p2-header.jpg');
    expect(fallback).toContain('https://cdn.example.com/p2-small.jpg');
    expect(fallback).toContain('https://cdn.example.com/p2-cover.jpg');
    expect(fallback).not.toContain('https://cdn.example.com/p2-header.jpg');
  });

  it('promotes fallback to HQ when a game only has a single non-HQ image', () => {
    const game = {
      id: 'indie-game',
      title: 'Indie Game',
      cover_url: 'https://cdn.example.com/indie-cover.jpg',
    };
    const { hq, fallback } = getThumbnailCandidates(game);

    expect(hq).toEqual(['https://cdn.example.com/indie-cover.jpg']);
    expect(fallback).toEqual([]);
  });

  it('extracts active pair for concurrent loading and advances when URLs fail', () => {
    const game = {
      id: 'steam-480',
      title: 'Spacewar',
      banner_url: 'https://cdn.example.com/bad-header.jpg',
      tiny_image: 'https://cdn.example.com/tiny.jpg',
    };

    const initialPair = getActiveThumbnailPair(game);
    expect(initialPair.activeHqUrl).toBe('https://cdn.example.com/bad-header.jpg');
    expect(initialPair.activeFallbackUrl).toBe('https://cdn.example.com/tiny.jpg');

    // If banner_url fails, next HQ candidate is chosen from Steam CDN header
    const failedHq = new Set(['https://cdn.example.com/bad-header.jpg']);
    const pairAfterHqFail = getActiveThumbnailPair(game, null, failedHq);
    expect(pairAfterHqFail.activeHqUrl).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/header.jpg'
    );
    expect(pairAfterHqFail.activeFallbackUrl).toBe('https://cdn.example.com/tiny.jpg');

    // If tiny fails too, advances to next fallback candidate
    const failedBoth = new Set([
      'https://cdn.example.com/bad-header.jpg',
      'https://cdn.example.com/tiny.jpg',
    ]);
    const pairAfterBothFail = getActiveThumbnailPair(game, null, failedBoth);
    expect(pairAfterBothFail.activeHqUrl).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/header.jpg'
    );
    expect(pairAfterBothFail.activeFallbackUrl).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/capsule_231x87.jpg'
    );
  });

  describe('getActiveDisplayThumbnail resolution', () => {
    const hqUrl = 'https://cdn.example.com/hq.jpg';
    const fallbackUrl = 'https://cdn.example.com/fallback.jpg';

    it('returns none when neither image is loaded yet', () => {
      const display = getActiveDisplayThumbnail({
        activeHqUrl: hqUrl,
        activeFallbackUrl: fallbackUrl,
        isHqLoaded: false,
        isFallbackLoaded: false,
      });
      expect(display).toEqual({ url: null, type: 'none' });
    });

    it('shows fallback when fallback loads first', () => {
      const display = getActiveDisplayThumbnail({
        activeHqUrl: hqUrl,
        activeFallbackUrl: fallbackUrl,
        isHqLoaded: false,
        isFallbackLoaded: true,
      });
      expect(display).toEqual({ url: fallbackUrl, type: 'fallback' });
    });

    it('changes to HQ when HQ becomes available and loaded after fallback', () => {
      const display = getActiveDisplayThumbnail({
        activeHqUrl: hqUrl,
        activeFallbackUrl: fallbackUrl,
        isHqLoaded: true,
        isFallbackLoaded: true,
      });
      expect(display).toEqual({ url: hqUrl, type: 'hq' });
    });

    it('shows HQ directly when HQ loads first, and remains HQ if fallback loads later', () => {
      const displayHqFirst = getActiveDisplayThumbnail({
        activeHqUrl: hqUrl,
        activeFallbackUrl: fallbackUrl,
        isHqLoaded: true,
        isFallbackLoaded: false,
      });
      expect(displayHqFirst).toEqual({ url: hqUrl, type: 'hq' });

      const displayLaterFallback = getActiveDisplayThumbnail({
        activeHqUrl: hqUrl,
        activeFallbackUrl: fallbackUrl,
        isHqLoaded: true,
        isFallbackLoaded: true,
      });
      expect(displayLaterFallback).toEqual({ url: hqUrl, type: 'hq' });
    });

    it('keeps fallback displayed when HQ fails to load', () => {
      const display = getActiveDisplayThumbnail({
        activeHqUrl: null,
        activeFallbackUrl: fallbackUrl,
        isHqLoaded: false,
        isFallbackLoaded: true,
      });
      expect(display).toEqual({ url: fallbackUrl, type: 'fallback' });
    });
  });
});
