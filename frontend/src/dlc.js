export function dlcTransferStatus(job) {
  return String(job?.status || '').toLowerCase();
}

export function dlcTransferInstalling(job) {
  return dlcTransferStatus(job) === 'installing'
    || String(job?.install_state || '').toLowerCase() === 'installing';
}

export function dlcTransferActive(job) {
  return ['running', 'downloading', 'installing', 'working', 'queued', 'pending'].includes(dlcTransferStatus(job));
}

export function isDlcTransferJob(job) {
  return job?.kind?.toLowerCase() === 'dlc'
    && String(job.target || '').replaceAll('\\', '/').includes('/dlc-downloads/');
}

export function dlcTransfer(jobs, gameId, addonId) {
  const segment = `/dlc-downloads/${gameId}/${addonId}/`;
  const matches = jobs.filter((job) => isDlcTransferJob(job)
    && String(job.target || '').replaceAll('\\', '/').includes(segment)
    && (dlcTransferActive(job) || ['paused', 'failed', 'interrupted', 'cancelled', 'canceled'].includes(dlcTransferStatus(job))));
  return matches.find(dlcTransferActive) || matches.find((job) => dlcTransferStatus(job) === 'paused') || matches[0];
}

export function filterDlc(addons, query, filter) {
  const search = query.trim().toLowerCase();
  return addons.filter((addon) => {
    const matches = `${addon.title} ${addon.id}`.toLowerCase().includes(search);
    return matches && (filter === 'all'
      || (filter === 'installed' && addon.installed)
      || (filter === 'available' && !addon.installed && !addon.interrupted && addon.content_available)
      || (filter === 'recovery' && addon.interrupted));
  });
}

export function dlcProgress(job) {
  const bytes = job?.bytes ?? job?.progress_bytes ?? 0;
  const total = job?.total_bytes ?? 0;
  return total > 0 ? Math.min(100, Math.max(0, Math.floor(bytes * 100 / total))) : null;
}
