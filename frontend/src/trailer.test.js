import { describe, expect, it } from 'vitest';
import { lowerTrailerLevel, pickTrailerLevel, trailerKey } from './trailer.js';

const steamLevels = [{ height: 360 }, { height: 480 }, { height: 720 }, { height: 1080 }];

describe('trailer rendition selection', () => {
  it('locks to at least 720p for small players', () => {
    expect(pickTrailerLevel(steamLevels, 300, 1)).toBe(2);
  });

  it('uses the full player height on high-density displays', () => {
    expect(pickTrailerLevel(steamLevels, 540, 2)).toBe(3);
  });

  it('never picks a rendition taller than needed when a smaller one fits', () => {
    expect(pickTrailerLevel([{ height: 1080 }, { height: 528 }, { height: 480 }], 400, 1)).toBe(1);
  });

  it('falls back to the smallest rendition when all exceed the need', () => {
    expect(pickTrailerLevel([{ height: 2160 }, { height: 1440 }], 300, 1)).toBe(1);
  });

  it('reports missing levels', () => {
    expect(pickTrailerLevel([], 500, 1)).toBe(-1);
    expect(pickTrailerLevel(null)).toBe(-1);
  });

  it('steps down to the next lower rendition only', () => {
    expect(lowerTrailerLevel(steamLevels, 3)).toBe(2);
    expect(lowerTrailerLevel(steamLevels, 0)).toBe(-1);
    expect(lowerTrailerLevel(steamLevels, 9)).toBe(-1);
  });
});

describe('trailer identity', () => {
  it('is stable across equal media objects and ignores images', () => {
    const first = { type: 'video', hls: 'a.m3u8', mp4: 'a.mp4', src: 'a.mp4' };
    expect(trailerKey({ ...first })).toBe(trailerKey(first));
    expect(trailerKey({ ...first, hls: 'b.m3u8' })).not.toBe(trailerKey(first));
    expect(trailerKey({ type: 'image', full: 'x.jpg' })).toBe('');
  });
});
