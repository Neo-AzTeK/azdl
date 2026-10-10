import { resolveGameArtwork } from './api.js';

// Memory cache for resolved SteamGrid / Steam CDN metadata
const MAX_ARTWORK_CACHE_ENTRIES = 256;
const MAX_CONCURRENT_ARTWORK_LOOKUPS = 8;
const ARTWORK_CACHE_INDEX_KEY = 'azdl_art_index_v1';
const artworkCache = new Map();
const pendingRequests = new Map();
const subscribers = new Map();
const allArtworkKeys = Symbol('all-artwork-keys');
let persistentArtworkKeys = null;
let artworkLookupQueue = [];
let artworkLookupQueueHead = 0;
let activeArtworkLookups = 0;

function isGuessedSteamArtwork(entry) {
  const urls = [entry?.banner_url, entry?.cover_url].filter(Boolean);
  return urls.length > 0 && urls.every((url) =>
    /steamstatic\.com\/store_item_assets\/steam\/apps\/\d+\/(header\.jpg|library_600x900\.jpg)(?:\?.*)?$/i.test(url)
  );
}

function pumpArtworkLookupQueue() {
  while (
    activeArtworkLookups < MAX_CONCURRENT_ARTWORK_LOOKUPS &&
    artworkLookupQueueHead < artworkLookupQueue.length
  ) {
    const request = artworkLookupQueue[artworkLookupQueueHead++];
    activeArtworkLookups += 1;
    Promise.resolve()
      .then(() => resolveGameArtwork(request.query))
      .then(request.resolve, () => request.resolve(null))
      .finally(() => {
        activeArtworkLookups -= 1;
        pumpArtworkLookupQueue();
      });
  }

  if (artworkLookupQueueHead === artworkLookupQueue.length) {
    artworkLookupQueue = [];
    artworkLookupQueueHead = 0;
  } else if (artworkLookupQueueHead > 64) {
    artworkLookupQueue = artworkLookupQueue.slice(artworkLookupQueueHead);
    artworkLookupQueueHead = 0;
  }
}

function resolveArtworkWithLimit(query) {
  return new Promise((resolve) => {
    artworkLookupQueue.push({ query, resolve });
    pumpArtworkLookupQueue();
  });
}

/**
 * Normalizes query string for caching keys
 */
export function normalizeQuery(query) {
  if (!query) return '';
  return String(query).trim().toLowerCase();
}

function cacheArtwork(key, entry) {
  artworkCache.delete(key);
  artworkCache.set(key, entry);
  if (artworkCache.size > MAX_ARTWORK_CACHE_ENTRIES) {
    const oldestKey = artworkCache.keys().next().value;
    artworkCache.delete(oldestKey);
  }
}

function rememberPersistentArtwork(key) {
  try {
    const storage = window.localStorage;
    if (!storage) return;

    if (!persistentArtworkKeys) {
      const storedIndex = JSON.parse(storage.getItem(ARTWORK_CACHE_INDEX_KEY) || '[]');
      persistentArtworkKeys = Array.isArray(storedIndex)
        ? [...new Set(storedIndex.filter((item) => typeof item === 'string'))]
        : [];

      // Include older cache entries created before the bounded index existed.
      const knownKeys = new Set(persistentArtworkKeys);
      for (let index = 0; index < storage.length; index += 1) {
        const storageKey = storage.key(index);
        if (storageKey?.startsWith('azdl_art_') && storageKey !== ARTWORK_CACHE_INDEX_KEY) {
          const artworkKey = storageKey.slice('azdl_art_'.length);
          if (!knownKeys.has(artworkKey)) {
            knownKeys.add(artworkKey);
            persistentArtworkKeys.push(artworkKey);
          }
        }
      }
    }

    persistentArtworkKeys = persistentArtworkKeys.filter((item) => item !== key);
    persistentArtworkKeys.push(key);
    while (persistentArtworkKeys.length > MAX_ARTWORK_CACHE_ENTRIES) {
      const evictedKey = persistentArtworkKeys.shift();
      storage.removeItem(`azdl_art_${evictedKey}`);
    }
    storage.setItem(ARTWORK_CACHE_INDEX_KEY, JSON.stringify(persistentArtworkKeys));
  } catch {
    // Ignore storage errors; artwork caching is optional.
  }
}

/**
 * Extracts numeric Steam App ID from game or job object
 */
export function extractSteamAppId(item) {
  if (!item) return null;
  if (item.steam_app_id) return String(item.steam_app_id);

  // 1. Explicit Steam-prefixed URLs or fields (steam://12345, steam-12345, steam:12345)
  const explicit = [
    item.url,
    item.install?.url,
    item.install?.game_id,
    item.install?.id,
    item.origin,
    item.qualified_id,
    item.game_id,
    item.id,
    item.target,
  ];

  for (const field of explicit) {
    if (!field) continue;
    const str = String(field);
    const match = str.match(/steam[-:\/]+(\d{3,9})/i) || str.match(/[:\-_](\d{3,9})$/);
    if (match) return match[1];
  }

  // 2. Purely numeric IDs (e.g. "1073440" or "480")
  for (const field of [item.steam_app_id, item.game_id, item.install?.id, item.id]) {
    if (field && /^\d{3,9}$/.test(String(field).trim())) {
      return String(field).trim();
    }
  }

  return null;
}

/**
 * Generates direct SteamGrid & Steam CDN asset URLs for a known App ID
 */
export function getSteamGridUrls(appId) {
  if (!appId) return null;
  const id = String(appId).trim();
  return {
    steam_app_id: id,
    banner_url: `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${id}/header.jpg`,
    capsule_url: `https://cdn.cloudflare.steamstatic.com/steam/apps/${id}/capsule_616x353.jpg`,
    cover_url: `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${id}/library_600x900.jpg`,
    hero_url: `https://cdn.cloudflare.steamstatic.com/steam/apps/${id}/hero_capsule.jpg`,
  };
}

/**
 * Reads artwork from memory cache or localStorage
 */
export function getResolvedArtwork(query) {
  const key = normalizeQuery(query);
  if (!key) return null;

  if (artworkCache.has(key)) {
    const entry = artworkCache.get(key);
    if (isGuessedSteamArtwork(entry)) {
      artworkCache.delete(key);
    } else {
    cacheArtwork(key, entry);
    return entry;
    }
  }

  // Check localStorage if available
  if (typeof window !== 'undefined' && window.localStorage) {
    try {
      const stored = window.localStorage.getItem(`azdl_art_${key}`);
      if (stored) {
        const parsed = JSON.parse(stored);
        if (isGuessedSteamArtwork(parsed)) {
          window.localStorage.removeItem(`azdl_art_${key}`);
          return null;
        }
        cacheArtwork(key, parsed);
        return parsed;
      }
    } catch {
      // Ignore localStorage errors
    }
  }

  return null;
}

/**
 * Saves artwork into memory cache and localStorage, notifying subscribers
 */
export function setResolvedArtwork(query, entry) {
  const key = normalizeQuery(query);
  if (!key || !entry) return;

  cacheArtwork(key, entry);

  if (typeof window !== 'undefined' && window.localStorage) {
    try {
      rememberPersistentArtwork(key);
      window.localStorage.setItem(`azdl_art_${key}`, JSON.stringify(entry));
    } catch {
      // Ignore quota/storage errors
    }
  }

  notifyArtworkSubscribers(subscribers.get(allArtworkKeys), key, entry);
  notifyArtworkSubscribers(subscribers.get(key), key, entry);
}

function notifyArtworkSubscribers(listeners, key, entry) {
  if (!listeners) return;
  for (const callback of listeners) {
    try {
      callback(key, entry);
    } catch (e) {
      console.error('Error in artwork subscriber:', e);
    }
  }
}

/**
 * Subscribes to artwork resolution events for one or more query keys.
 */
export function subscribeArtwork(callback, queries = null) {
  const keys = queries === null
    ? [allArtworkKeys]
    : [...new Set((Array.isArray(queries) ? queries : [queries]).map(normalizeQuery).filter(Boolean))];
  const buckets = [];

  for (const key of keys) {
    let listeners = subscribers.get(key);
    if (!listeners) {
      listeners = new Set();
      subscribers.set(key, listeners);
    }
    listeners.add(callback);
    buckets.push([key, listeners]);
  }

  return () => {
    for (const [key, listeners] of buckets) {
      listeners.delete(callback);
      if (listeners.size === 0) subscribers.delete(key);
    }
  };
}

/**
 * Resolves artwork asynchronously from Steam / SteamGrid without waiting for catalog loading
 */
export async function fetchArtwork(query) {
  const key = normalizeQuery(query);
  if (!key) return null;

  // 1. Check existing cache
  const cached = getResolvedArtwork(key);
  if (cached) return cached;

  // 2. Direct fast-path if query is numeric Steam App ID or steam: prefixed
  const appId = extractSteamAppId({ id: key });
  if (appId) {
    const direct = getSteamGridUrls(appId);
    setResolvedArtwork(key, direct);
    return direct;
  }

  // 3. Deduplicate in-flight requests
  if (pendingRequests.has(key)) {
    return await pendingRequests.get(key);
  }

  const promise = (async () => {
    try {
      const result = await resolveArtworkWithLimit(key);
      if (result && (result.banner_url || result.cover_url || result.steam_app_id)) {
        const entry = {
          steam_app_id: result.steam_app_id ? String(result.steam_app_id) : null,
          banner_url: result.banner_url || (result.steam_app_id ? getSteamGridUrls(result.steam_app_id).banner_url : null),
          cover_url: result.cover_url || (result.steam_app_id ? getSteamGridUrls(result.steam_app_id).cover_url : null),
          capsule_url: result.steam_app_id ? getSteamGridUrls(result.steam_app_id).capsule_url : null,
        };
        setResolvedArtwork(key, entry);
        return entry;
      }
      const emptyEntry = { steam_app_id: null, banner_url: null, cover_url: null, capsule_url: null };
      setResolvedArtwork(key, emptyEntry);
      return null;
    } catch (e) {
      console.warn(`Failed to resolve artwork for query "${key}":`, e);
      const emptyEntry = { steam_app_id: null, banner_url: null, cover_url: null, capsule_url: null };
      setResolvedArtwork(key, emptyEntry);
    } finally {
      pendingRequests.delete(key);
    }
    return null;
  })();

  pendingRequests.set(key, promise);
  return await promise;
}

/**
 * Derives ordered thumbnail candidates for a game item
 */
export function getCandidateThumbnails(game, resolvedArtwork = null) {
  if (!game) return [];
  const list = [];

  const add = (url) => {
    if (url && typeof url === 'string') {
      const clean = url.trim();
      if (clean && !list.includes(clean)) {
        list.push(clean);
      }
    }
  };

  // 1. Regular size banners / headers provided directly by game / catalog
  add(game.banner_url);
  add(game.header_image);
  add(game.large_capsule_image);
  add(game.capsule_url);

  // 2. Resolved artwork banner & capsule
  if (resolvedArtwork?.banner_url) add(resolvedArtwork.banner_url);
  if (resolvedArtwork?.capsule_url) add(resolvedArtwork.capsule_url);

  // 3. Direct Steam CDN regular-size header & large capsule
  const appId = extractSteamAppId(game) || resolvedArtwork?.steam_app_id;
  if (appId) {
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/header.jpg`);
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/header.jpg`);
    add(`https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/${appId}/header.jpg`);
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/capsule_616x353.jpg`);
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/capsule_616x353.jpg`);
    add(`https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/${appId}/capsule_616x353.jpg`);
  }

  // 4. Smaller capsules & alternate sizes if regular size is unavailable
  add(game.tiny_image);
  add(game.small_capsule_image);
  add(game.capsule_image);
  add(game.thumbnail_url);

  if (appId) {
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/capsule_231x87.jpg`);
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/capsule_231x87.jpg`);
    add(`https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/${appId}/capsule_231x87.jpg`);
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/capsule_184x69.jpg`);
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/capsule_184x69.jpg`);
  }

  // 5. Vertical portrait cover art & hero capsule
  add(game.cover_url);
  if (resolvedArtwork?.cover_url) add(resolvedArtwork.cover_url);
  if (resolvedArtwork?.hero_url) add(resolvedArtwork.hero_url);

  if (appId) {
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/library_600x900.jpg`);
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/library_600x900.jpg`);
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/hero_capsule.jpg`);
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/hero_capsule.jpg`);
  }

  // 6. Generated background page art as ultimate fallback
  if (appId) {
    add(`https://cdn.cloudflare.steamstatic.com/steam/apps/${appId}/page_bg_generated_v6b.jpg`);
    add(`https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/page_bg_generated_v6b.jpg`);
  }

  return list;
}

/**
 * Derives best available thumbnail for a transfer job
 */
export function getJobThumbnail(job, resolvedArtwork = null) {
  if (!job) return null;
  if (job.install?.banner_url) return job.install.banner_url;
  if (job.install?.header_image) return job.install.header_image;
  if (job.install?.large_capsule_image) return job.install.large_capsule_image;
  if (job.install?.cover_url) return job.install.cover_url;
  if (job.install?.tiny_image) return job.install.tiny_image;
  if (job.banner_url) return job.banner_url;
  if (job.cover_url) return job.cover_url;
  if (resolvedArtwork?.banner_url) return resolvedArtwork.banner_url;
  if (resolvedArtwork?.cover_url) return resolvedArtwork.cover_url;

  const appId = extractSteamAppId(job) || resolvedArtwork?.steam_app_id;
  if (appId) {
    return `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${appId}/header.jpg`;
  }
  return null;
}
