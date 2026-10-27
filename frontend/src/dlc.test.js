import { describe, expect, it } from 'vitest';
import { dlcTransfer, dlcTransferActive, dlcTransferStatus, dlcTransferInstalling, isDlcTransferJob, dlcArchiveIdentity, dlcProgress, filterDlc } from './dlc.js';

describe('DLC page', () => {
  const addons = [
    { id: '10', title: 'Bonus', installed: true },
    { id: '20', title: 'Expansion', content_available: true },
    { id: '30', title: 'Soundtrack', interrupted: true },
  ];
  it('filters by title, ID and actionable state', () => {
    expect(filterDlc(addons, 'EXP', 'available')).toEqual([addons[1]]);
    expect(filterDlc(addons, '10', 'installed')).toEqual([addons[0]]);
    expect(filterDlc(addons, '', 'recovery')).toEqual([addons[2]]);
    expect(filterDlc(addons, 'Bonus', 'available')).toEqual([]);
  });
  it('matches isolated Windows and Linux staging paths without ID prefix collisions', () => {
    const job = { kind: 'Dlc', status: 'running', target: 'C:\\azdl\\dlc-downloads\\base\\20\\payload' };
    expect(dlcTransfer([job], 'base', 20)).toBe(job);
    expect(dlcTransfer([job], 'base', 2)).toBeUndefined();
    expect(dlcTransfer([job], 'other', 20)).toBeUndefined();
    expect(dlcTransfer([{ ...job, target: '/tmp/dlc-downloads/base/20/bonus.zip' }], 'base', '20')).toBeDefined();
  });
  it('does not fabricate progress when totals are unknown', () => {
    expect(dlcProgress({ bytes: 80 })).toBeNull();
    expect(dlcProgress({ progress_bytes: 50, total_bytes: 100 })).toBe(50);
    expect(dlcProgress({ bytes: 150, total_bytes: 100 })).toBe(100);
  });
  it('recognizes the installation phase of both depot and archive jobs', () => {
    expect(dlcTransferInstalling({ kind: 'Dlc', status: 'Installing', install_state: 'Pending' })).toBe(true);
    expect(dlcTransferInstalling({ kind: 'Archive', status: 'Complete', install_state: 'Installing' })).toBe(true);
    expect(dlcTransferInstalling({ status: 'Downloading', install_state: 'Pending' })).toBe(false);
    expect(dlcTransferInstalling(undefined)).toBe(false);
  });
  it('recognizes the desktop API status labels and prioritizes active multipart transfers', () => {
    const target = '/tmp/dlc-downloads/base/20/part.rar';
    const failed = { kind: 'Dlc', status: 'Failed', target };
    const downloading = { kind: 'Dlc', status: 'Downloading', target: `${target}.next` };
    const installing = { kind: 'Dlc', status: 'Installing', install_state: 'Pending', target };
    expect(dlcTransfer([failed, downloading], 'base', '20')).toBe(downloading);
    expect(dlcTransfer([installing], 'base', '20')).toBe(installing);
    expect(dlcTransferActive(installing)).toBe(true);
    expect(dlcTransferStatus(failed)).toBe('failed');
    expect(dlcTransfer([{ ...failed, status: 'Cancelled' }], 'base', '20')).toBeDefined();
    expect(dlcTransferStatus(dlcTransfer([{ ...failed, status: 'Interrupted' }], 'base', '20'))).toBe('interrupted');
  });
  it('recognizes catalog archive DLC without misclassifying ordinary game archives', () => {
    const archive = { kind: 'Archive', status: 'Downloading', target: '/tmp/dlc-downloads/base/20/bonus.zip' };
    expect(isDlcTransferJob(archive)).toBe(true);
    expect(dlcTransfer([archive], 'base', '20')).toBe(archive);
    expect(isDlcTransferJob({ ...archive, target: '/tmp/games/base/bonus.zip' })).toBe(false);
    expect(isDlcTransferJob({ ...archive, kind: 'Patch' })).toBe(false);
    expect(dlcArchiveIdentity(archive)).toEqual({ gameId: 'base', addonId: '20' });
    expect(dlcArchiveIdentity({ ...archive, target: 'C:\\azdl\\dlc-downloads\\base\\20\\bonus.zip' })).toEqual({ gameId: 'base', addonId: '20' });
    expect(dlcArchiveIdentity({ ...archive, target: '/tmp/dlc-downloads/base/../bonus.zip' })).toBeNull();
    expect(dlcArchiveIdentity({ ...archive, target: '/tmp/dlc-downloads/base/20/nested/bonus.zip' })).toBeNull();
  });
});
