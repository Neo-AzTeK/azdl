import { getSteamDetails } from './api.js';

const MAX_CONCURRENT_LOOKUPS = 4;
const MAX_CACHE_ENTRIES = 256;
const SUCCESS_TTL_MS = 60 * 60 * 1000;
const MISS_TTL_MS = 5 * 60 * 1000;
const cache = new Map();
const pending = new Map();
const queue = [];
let activeLookups = 0;

function remember(appId, size) {
  cache.delete(appId);
  cache.set(appId, {
    size,
    expiresAt: Date.now() + (size ? SUCCESS_TTL_MS : MISS_TTL_MS),
  });
  if (cache.size > MAX_CACHE_ENTRIES) cache.delete(cache.keys().next().value);
}

function pump() {
  while (activeLookups < MAX_CONCURRENT_LOOKUPS && queue.length) {
    const request = queue.shift();
    activeLookups += 1;
    Promise.resolve()
      .then(() => getSteamDetails(request.appId))
      .then((details) => typeof details?.storage_size === 'string' ? details.storage_size : null)
      .catch(() => null)
      .then((size) => {
        remember(request.appId, size);
        request.resolve(size);
      })
      .finally(() => {
        pending.delete(request.appId);
        activeLookups -= 1;
        pump();
      });
  }
}

/** Fetch Steam's advertised PC storage requirement with bounded concurrency. */
export function getSteamStorageSize(appId) {
  const id = Number(appId);
  if (!Number.isSafeInteger(id) || id <= 0) return Promise.resolve(null);

  const cached = cache.get(id);
  if (cached && cached.expiresAt > Date.now()) return Promise.resolve(cached.size);
  if (cached) cache.delete(id);
  if (pending.has(id)) return pending.get(id);

  const lookup = new Promise((resolve) => {
    queue.push({ appId: id, resolve });
    pump();
  });
  pending.set(id, lookup);
  return lookup;
}
