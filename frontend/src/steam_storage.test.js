import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getSteamDetails } from './api.js';
import { getSteamStorageSize } from './steam_storage.js';

vi.mock('./api.js', () => ({ getSteamDetails: vi.fn() }));

describe('Steam storage lookups', () => {
  beforeEach(() => {
    getSteamDetails.mockReset();
  });

  it('deduplicates requests and caches the Store requirement', async () => {
    getSteamDetails.mockResolvedValue({ storage_size: '20 GB' });

    const first = getSteamStorageSize(980001);
    const second = getSteamStorageSize(980001);
    expect(first).toBe(second);
    expect(await first).toBe('20 GB');
    expect(await getSteamStorageSize(980001)).toBe('20 GB');
    expect(getSteamDetails).toHaveBeenCalledTimes(1);
  });

  it('limits concurrent Store detail requests', async () => {
    const releases = [];
    getSteamDetails.mockImplementation(() => new Promise((resolve) => releases.push(resolve)));
    const lookups = [980101, 980102, 980103, 980104, 980105]
      .map((id) => getSteamStorageSize(id));

    await vi.waitFor(() => expect(getSteamDetails).toHaveBeenCalledTimes(4));
    releases[0]({ storage_size: '1 GB' });
    await vi.waitFor(() => expect(getSteamDetails).toHaveBeenCalledTimes(5));
    for (const release of releases.slice(1)) release({ storage_size: '2 GB' });
    expect(await Promise.all(lookups)).toEqual(['1 GB', '2 GB', '2 GB', '2 GB', '2 GB']);
  });
});
