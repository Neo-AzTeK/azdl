import { describe, it, expect } from 'vitest';
import {
  parseSizeToBytes,
  estimateJobTotalBytes,
  calculateJobProgressPercent,
  getEffectiveJobTotal,
} from './transfers.js';

describe('transfers utility', () => {
  it('parses size strings into byte values correctly', () => {
    expect(parseSizeToBytes('20 GB')).toBe(20 * 1024 * 1024 * 1024);
    expect(parseSizeToBytes('500 MB')).toBe(500 * 1024 * 1024);
    expect(parseSizeToBytes('1.5 TB')).toBe(1.5 * 1024 * 1024 * 1024 * 1024);
    expect(parseSizeToBytes('30 GB available space')).toBe(30 * 1024 * 1024 * 1024);
    expect(parseSizeToBytes('512 KB')).toBe(512 * 1024);
    expect(parseSizeToBytes('1024 B')).toBe(1024);
    expect(parseSizeToBytes(1048576)).toBe(1048576);
    expect(parseSizeToBytes(null)).toBeNull();
    expect(parseSizeToBytes('invalid')).toBeNull();
  });

  it('estimates job total bytes from metadata and fallback game', () => {
    const jobWithExplicit = { total_bytes: 5000 };
    expect(estimateJobTotalBytes(jobWithExplicit)).toBe(5000);

    const jobWithEstimated = { estimated_bytes: 4000 };
    expect(estimateJobTotalBytes(jobWithEstimated)).toBe(4000);

    const jobWithInstallSize = { install: { file_size: '2 GB' } };
    expect(estimateJobTotalBytes(jobWithInstallSize)).toBe(2 * 1024 * 1024 * 1024);

    const jobWithoutTotal = { bytes: 2000000 };
    const game = { storage_size: '10 GB' };
    expect(estimateJobTotalBytes(jobWithoutTotal, game)).toBe(10 * 1024 * 1024 * 1024);

    const jobNoMetadata = { bytes: 500000000 };
    expect(estimateJobTotalBytes(jobNoMetadata)).toBe(Math.round(500000000 * 1.2));
  });

  it('calculates job progress percent accurately', () => {
    expect(calculateJobProgressPercent({ status: 'complete' })).toBe(100);
    expect(calculateJobProgressPercent({ progress: 75.4 })).toBe(75);
    expect(calculateJobProgressPercent({ bytes: 250, total_bytes: 1000 })).toBe(25);
    expect(calculateJobProgressPercent({ bytes: 500 }, 1000)).toBe(50);
    expect(calculateJobProgressPercent({ bytes: 1000, total_bytes: 1000 })).toBe(99); // caps at 99 before complete
    expect(calculateJobProgressPercent({ bytes: 10, total_bytes: 0 })).toBe(1);
    expect(calculateJobProgressPercent(null)).toBe(0);
  });

  it('resolves effective job total using dynamic estimated map', () => {
    const job = { id: 'j1', bytes: 100 };
    const map = { j1: 20 * 1024 * 1024 * 1024 };
    expect(getEffectiveJobTotal(job, null, map)).toBe(20 * 1024 * 1024 * 1024);
  });
});


describe('global transfer activity', () => {
  it('keeps background transfers active but excludes paused and failed native jobs', async () => {
    const { isTransferActive } = await import('./transfers.js');
    expect(isTransferActive({ status: 'Downloading', install_state: 'Pending' })).toBe(true);
    expect(isTransferActive({ status: 'Installing' })).toBe(true);
    for (const status of ['Paused', 'Failed', 'Interrupted', 'Cancelled']) {
      expect(isTransferActive({ status, install_state: 'Pending' })).toBe(false);
    }
  });
});
