import { describe, expect, it } from 'vitest';
import { displayPath } from './paths.js';

describe('storage path display', () => {
  it('shows readable drive and network paths returned by Windows canonicalization', () => {
    expect(displayPath('\\\\?\\C:\\Users\\René\\Games')).toBe('C:\\Users\\René\\Games');
    expect(displayPath('\\\\?\\D:\\')).toBe('D:\\');
    expect(displayPath('\\\\?\\UNC\\server\\Games\\Moon Sweepers')).toBe('\\\\server\\Games\\Moon Sweepers');
  });

  it('preserves ordinary Windows and Linux paths and handles missing values', () => {
    for (const path of ['D:\\Games', '\\\\server\\Games', '/mnt/games', '/home/player/Games']) {
      expect(displayPath(path)).toBe(path);
    }
    expect(displayPath(null)).toBe('');
    expect(displayPath(undefined)).toBe('');
  });
});
