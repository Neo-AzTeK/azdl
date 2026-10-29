// Trailer playback policy for Steam HLS streams.
//
// WebKitGTK decodes MSE through GStreamer, which reconfigures the decoder on
// every resolution change; adaptive switches mid-playback show up as freezes
// and frames jumping backwards. Trailers therefore play one fixed rendition,
// chosen before the first fragment loads, and only step down on repeated stalls.

/** hls.js options: bounded buffers so long trailers never trigger quota flushes. */
export const TRAILER_HLS_CONFIG = Object.freeze({
  enableWorker: true,
  lowLatencyMode: false,
  maxBufferLength: 30,
  maxMaxBufferLength: 60,
  backBufferLength: 30,
});

/** Lowest rendition height worth locking to, regardless of player size. */
export const MIN_TRAILER_HEIGHT = 720;

/** Non-fatal stalls tolerated on one rendition before stepping down a level. */
export const STALLS_BEFORE_STEP_DOWN = 3;

/**
 * Pick the highest rendition no taller than the player needs.
 * Returns -1 when the manifest has no usable levels.
 */
export function pickTrailerLevel(levels, playerHeight = 0, pixelRatio = 1) {
  if (!Array.isArray(levels) || levels.length === 0) return -1;
  const needed = Math.max(MIN_TRAILER_HEIGHT, Math.ceil((Number(playerHeight) || 0) * (Number(pixelRatio) || 1)));
  let best = -1;
  let smallest = 0;
  levels.forEach((level, index) => {
    const height = Number(level?.height) || 0;
    if (height < (Number(levels[smallest]?.height) || 0)) smallest = index;
    if (height > 0 && height <= needed && (best < 0 || height > Number(levels[best].height))) best = index;
  });
  return best >= 0 ? best : smallest;
}

/** Index of the next lower rendition, or -1 when already at the lowest. */
export function lowerTrailerLevel(levels, current) {
  if (!Array.isArray(levels) || !levels[current]) return -1;
  const currentHeight = Number(levels[current].height) || 0;
  let lower = -1;
  levels.forEach((level, index) => {
    const height = Number(level?.height) || 0;
    if (height < currentHeight && (lower < 0 || height > Number(levels[lower].height))) lower = index;
  });
  return lower;
}

/** Stable identity of a trailer source, so unrelated re-renders never restart playback. */
export function trailerKey(media) {
  if (!media || media.type !== 'video') return '';
  return [media.hls, media.mp4, media.webm, media.src].map((value) => value || '').join('|');
}
