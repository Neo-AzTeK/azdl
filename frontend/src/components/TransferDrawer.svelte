<script>
  import { Icon } from 'm3-svelte';
  import iconCloudDownload from '@ktibow/iconset-material-symbols/cloud-download-outline';
  import iconExpandLess from '@ktibow/iconset-material-symbols/expand-less';
  import iconVideogameAsset from '@ktibow/iconset-material-symbols/videogame-asset-outline';
  import { onDestroy } from 'svelte';
  import { runAction } from '../api.js';
  import { notify } from '../notifications.js';
  import {
    getJobThumbnail as resolveJobThumb,
    getResolvedArtwork,
    fetchArtwork,
    normalizeQuery,
    subscribeArtwork,
    extractSteamAppId,
  } from '../covers.js';
  import {
    parseSizeToBytes,
    calculateJobProgressPercent,
    getEffectiveJobTotal,
  } from '../transfers.js';
  import { getSteamStorageSize } from '../steam_storage.js';

  let { jobs = [], onRefresh = null, onDlcChanged = null } = $props();

  let expanded = $state(false);
  let brokenThumbs = $state(new Set());
  let artworkRevision = $state(0);

  $effect(() => {
    const queries = jobs.map((job) =>
      normalizeQuery(job.install?.title || job.title || job.target)
    );
    return subscribeArtwork(() => {
      artworkRevision++;
    }, queries);
  });

  function formatJobStatus(status, installState) {
    if (!status) return 'Downloading';
    const s = status.toLowerCase();
    if (s === 'running') {
      if (installState && installState.toLowerCase() === 'installing') {
        return 'Installing';
      }
      return 'Downloading';
    }
    return status;
  }

  function getJobThumbnail(job) {
    if (!job) return null;
    const _rev = artworkRevision;
    const query = job.install?.title || job.title || job.target;
    const resolved = query ? getResolvedArtwork(query) : null;
    const thumb = resolveJobThumb(job, resolved);
    if (!thumb && query && resolved === null) {
      fetchArtwork(query);
    }
    return thumb;
  }

  function handleThumbError(id) {
    brokenThumbs.add(id);
    brokenThumbs = new Set(brokenThumbs);
  }

  function isJobActive(status) {
    if (!status) return false;
    const s = status.toLowerCase();
    return ['downloading', 'installing', 'working', 'queued', 'pending', 'running'].includes(s);
  }

  function isJobPaused(status) {
    if (!status) return false;
    return status.toLowerCase() === 'paused';
  }

  function isJobFinished(status) {
    if (!status) return false;
    const s = status.toLowerCase();
    return ['complete', 'completed', 'finished', 'cancelled', 'canceled', 'failed'].includes(s);
  }

  // Filter jobs into active vs finished
  let activeJobs = $derived(
    jobs.filter((j) => isJobActive(j.status))
  );

  let finishedJobs = $derived(
    jobs.filter((j) => isJobFinished(j.status))
  );

  let successfulJobs = $derived(
    jobs.filter((j) => ['complete', 'cancelled'].includes((j.status || '').toLowerCase()))
  );

  let totalActiveBytes = $derived(
    activeJobs.reduce((acc, j) => acc + (j.bytes || 0), 0)
  );

  function formatBytes(bytes) {
    if (!bytes || bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
  }

  let estimatedSizes = $state({});

  $effect(() => {
    for (const job of activeJobs) {
      if (!job.total_bytes && !job.total && !estimatedSizes[job.id]) {
        const appId = extractSteamAppId(job);
        if (appId) {
          getSteamStorageSize(appId).then((size) => {
            if (size) {
              const parsed = parseSizeToBytes(size);
              if (parsed) {
                estimatedSizes = { ...estimatedSizes, [job.id]: parsed };
              }
            }
          });
        }
      }
    }
  });

  function getEffectiveTotal(job) {
    return getEffectiveJobTotal(job, null, estimatedSizes);
  }

  function getJobProgressPercent(job) {
    if (!job) return 0;
    const total = getEffectiveTotal(job);
    return calculateJobProgressPercent(job, total);
  }

  async function handleJobAction(action, id) {
    try {
      await runAction({ action: `job-${action}`, id });
    } catch (e) {
      notify.error(e?.message || `Failed to ${action} transfer`);
    } finally {
      if (action === 'resume') onDlcChanged?.();
      onRefresh?.();
    }
  }

  // Clear successful transfers while retaining failures for inspection.
  let autoClearTimer = null;

  $effect(() => {
    if (successfulJobs.length > 0) {
      if (!autoClearTimer) {
        autoClearTimer = setTimeout(async () => {
          autoClearTimer = null;
          try {
            await runAction({ action: 'job-clear-successful' });
            onRefresh?.();
          } catch (error) {
            console.error('Failed to clear successful jobs:', error);
          }
        }, 1500);
      }
    } else if (autoClearTimer) {
      clearTimeout(autoClearTimer);
      autoClearTimer = null;
    }
  });

  onDestroy(() => {
    if (autoClearTimer) {
      clearTimeout(autoClearTimer);
      autoClearTimer = null;
    }
  });

  async function handleClearCompleted() {
    try {
      await runAction({ action: 'job-clear-completed' });
      if (onRefresh) onRefresh();
    } catch (e) {
      console.error('Failed to clear completed jobs:', e);
    }
  }
</script>

<div class="transfer-drawer" class:expanded>
  <!-- Persistent Bottom Bar -->
  <button
    type="button"
    class="drawer-header"
    onclick={() => (expanded = !expanded)}
    aria-expanded={expanded}
  >
    {#if activeJobs.length > 0}
      {@const topPct = getJobProgressPercent(activeJobs[0])}
      {@const topTotal = getEffectiveTotal(activeJobs[0])}
      <div class="drawer-header-progress">
        <div
          class="drawer-header-progress-fill"
          class:indeterminate={topPct === 0 && !topTotal}
          style="width: {topPct > 0 ? topPct + '%' : (topTotal ? '1%' : '100%')}"
        ></div>
      </div>
    {/if}

    <div class="header-left">
      <div class="transfer-badge" class:has-active={activeJobs.length > 0}>
        <Icon icon={iconCloudDownload} size={16} />
        <span>
          {activeJobs.length > 0
            ? `${activeJobs.length} Active Transfer${activeJobs.length > 1 ? 's' : ''}`
            : 'Transfers'}
        </span>
      </div>

      {#if activeJobs.length > 0}
        {@const firstThumb = getJobThumbnail(activeJobs[0])}
        {@const firstPct = getJobProgressPercent(activeJobs[0])}
        <div class="mini-summary">
          {#if firstThumb && !brokenThumbs.has(activeJobs[0].id)}
            <img
              src={firstThumb}
              alt=""
              class="mini-thumb-img"
              loading="lazy"
              decoding="async"
              onerror={() => handleThumbError(activeJobs[0].id)}
            />
          {/if}
          <span class="active-title">
            {activeJobs[0].install?.title || activeJobs[0].target || 'Downloading...'}
          </span>
          {#if firstPct > 0}
            <span class="mini-pct">{firstPct}%</span>
          {/if}
          <span class="byte-count">{formatBytes(totalActiveBytes)}</span>
        </div>
      {:else}
        <span class="idle-text">{jobs.length > 0 ? 'All transfers finished' : 'No active transfers'}</span>
      {/if}
    </div>

    <div class="header-right">
      <div class="expand-btn" title={expanded ? 'Collapse drawer' : 'Expand drawer'}>
        <Icon icon={iconExpandLess} size={18} class={expanded ? 'chevron-icon rotate' : 'chevron-icon'} />
      </div>
    </div>
  </button>

  <!-- Expanded Drawer Content -->
  {#if expanded}
    <div class="drawer-content">
      <div class="drawer-toolbar">
        <span class="toolbar-title">
          {jobs.length} Transfer{jobs.length === 1 ? '' : 's'}
        </span>
        <div class="toolbar-actions-group">
          {#if finishedJobs.length > 0}
            <button class="clear-all-btn" onclick={handleClearCompleted}>
              Clear Finished ({finishedJobs.length})
            </button>
          {/if}
        </div>
      </div>

      <div class="jobs-list">
        {#if jobs.length === 0}
          <div class="empty-state">No download or install transfers recorded</div>
        {:else}
          {#each jobs as job}
            {@const thumbUrl = getJobThumbnail(job)}
            {@const displayStatus = formatJobStatus(job.status, job.install_state)}
            <div class="job-card">
              <div class="job-thumb">
                {#if thumbUrl && !brokenThumbs.has(job.id)}
                  <img
                    src={thumbUrl}
                    alt=""
                    class="job-thumb-img"
                    loading="lazy"
                    decoding="async"
                    onerror={() => handleThumbError(job.id)}
                  />
                {:else}
                  <div class="job-thumb-placeholder">
                    <Icon icon={iconVideogameAsset} size={16} />
                  </div>
                {/if}
              </div>

              <div class="job-meta">
                <div class="job-info">
                  <span class="job-target">
                    {job.install?.title || job.target || `Job ${job.id.substring(0, 8)}`}
                  </span>
                  <span class="job-kind-badge">{job.kind || 'Download'}</span>
                </div>

                <!-- Material 3 Transfer Progress Bar -->
                {#if isJobActive(job.status) || isJobPaused(job.status)}
                  {@const pct = getJobProgressPercent(job)}
                  {@const total = getEffectiveTotal(job)}
                  <div class="job-progress-wrap">
                    <div class="job-progress-track">
                      <div
                        class="job-progress-fill"
                        class:indeterminate={!total && pct === 0 && isJobActive(job.status)}
                        style="width: {pct > 0 ? pct + '%' : (total ? '1%' : '100%')}"
                      ></div>
                    </div>
                  </div>
                {/if}

                <div class="job-status-row">
                  <span class="job-status status-{displayStatus.toLowerCase()}">
                    {displayStatus}
                  </span>
                  {#if getEffectiveTotal(job) > 0}
                    {@const total = getEffectiveTotal(job)}
                    {@const pct = getJobProgressPercent(job)}
                    <span class="job-progress-fraction">
                      <strong>{pct}%</strong> ({formatBytes(job.bytes)} / {formatBytes(total)})
                    </span>
                  {:else}
                    <span class="job-bytes">{formatBytes(job.bytes)}</span>
                  {/if}
                </div>
              </div>

              {#if job.error}
                <div class="job-error-msg">{job.error}</div>
              {/if}

              <div class="job-actions">
                {#if isJobActive(job.status)}
                  {#if job.kind?.toLowerCase() !== 'dlc' && job.install_state?.toLowerCase() !== 'installing'}
                    <button class="action-btn" onclick={() => handleJobAction('pause', job.id)}>
                      Pause
                    </button>
                  {/if}
                  {#if job.install_state?.toLowerCase() !== 'installing'}
                    <button class="action-btn danger" onclick={() => handleJobAction('cancel', job.id)}>
                      Cancel
                    </button>
                  {/if}
                {:else if isJobPaused(job.status)}
                  {#if job.kind?.toLowerCase() !== 'dlc'}
                    <button class="action-btn primary" onclick={() => handleJobAction('resume', job.id)}>
                      Resume
                    </button>
                  {/if}
                  <button class="action-btn danger" onclick={() => handleJobAction('cancel', job.id)}>
                    Cancel
                  </button>
                {:else}
                  {#if job.kind?.toLowerCase() !== 'dlc' && ['failed', 'interrupted'].includes(job.status?.toLowerCase())}
                    <button class="action-btn primary" onclick={() => handleJobAction('resume', job.id)}>Retry</button>
                  {/if}
                  <button class="action-btn outlined" onclick={() => handleJobAction('clear', job.id)}>
                    Clear
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .transfer-drawer {
    background: var(--md-sys-color-surface-container);
    border-top: 1px solid var(--md-sys-color-outline-variant);
    display: flex;
    flex-direction: column;
    z-index: 80;
    transition: all 240ms cubic-bezier(0.2, 0, 0, 1);
  }

  .drawer-header {
    width: 100%;
    background: transparent;
    border: none;
    outline: none;
    color: inherit;
    font: inherit;
    height: 38px;
    padding: 0 16px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
    transition: background-color 150ms ease;
  }

  .drawer-header:hover {
    background: var(--md-sys-color-surface-container-high);
    border: none;
    outline: none;
  }

  .drawer-header:focus-visible {
    outline: 2px solid var(--md-sys-color-primary);
    outline-offset: -2px;
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .transfer-badge {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface-variant);
  }

  .transfer-badge.has-active {
    color: var(--md-sys-color-primary);
  }

  .mini-summary {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }

  .mini-thumb-img {
    width: 24px;
    height: 14px;
    aspect-ratio: 16 / 9;
    border-radius: 2px;
    object-fit: cover;
    display: block;
    flex-shrink: 0;
  }

  .active-title {
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--md-sys-color-on-surface);
  }

  .byte-count {
    color: var(--md-sys-color-on-surface-variant);
    font-size: 11px;
    background: var(--md-sys-color-surface-container-highest);
    padding: 1px 6px;
    border-radius: 6px;
  }

  .idle-text {
    font-size: 11px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .expand-btn {
    background: none;
    border: none;
    color: var(--md-sys-color-on-surface-variant);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    border-radius: 50%;
  }

  :global(.chevron-icon) {
    transition: transform 200ms ease;
  }

  :global(.chevron-icon.rotate) {
    transform: rotate(180deg);
  }

  .drawer-content {
    max-height: 260px;
    overflow-y: auto;
    padding: 8px 16px 12px;
    border-top: 1px solid var(--md-sys-color-outline-variant);
    background: var(--md-sys-color-surface-container-low);
  }

  .drawer-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 2px 8px;
  }

  .toolbar-title {
    font-size: 11px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface-variant);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .toolbar-actions-group {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .clear-all-btn {
    background: transparent;
    border: 1px solid var(--md-sys-color-outline-variant);
    color: var(--md-sys-color-on-surface-variant);
    border-radius: var(--md-sys-shape-small, 8px);
    padding: 3px 10px;
    font-size: 11px;
    font-weight: 500;
    cursor: pointer;
    transition: all 150ms ease;
  }

  .clear-all-btn:hover {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
  }

  .jobs-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .empty-state {
    padding: 24px;
    text-align: center;
    font-size: 12px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .job-card {
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: 10px;
    padding: 10px 14px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .job-thumb {
    width: 56px;
    height: 32px;
    aspect-ratio: 16 / 9;
    border-radius: var(--md-sys-shape-extra-small, 4px);
    overflow: hidden;
    background: var(--md-sys-color-surface-container-high);
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .job-thumb-img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .job-thumb-placeholder {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    background: linear-gradient(
      135deg,
      var(--md-sys-color-surface-container-high) 0%,
      var(--md-sys-color-surface-container-highest) 100%
    );
    color: var(--md-sys-color-primary);
    opacity: 0.7;
  }

  .job-meta {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
    overflow: hidden;
  }

  .job-info {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .job-target {
    font-size: 13px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .job-kind-badge {
    font-size: 10px;
    text-transform: uppercase;
    font-weight: 700;
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface-variant);
    padding: 1px 6px;
    border-radius: 4px;
  }

  .job-status-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .job-status {
    font-size: 11px;
    font-weight: 500;
  }

  .status-downloading,
  .status-installing,
  .status-working,
  .status-running,
  .status-queued,
  .status-pending {
    color: var(--md-sys-color-primary);
  }

  .status-complete,
  .status-completed,
  .status-finished {
    color: var(--md-sys-color-primary);
  }

  .status-paused {
    color: var(--md-sys-color-tertiary);
  }

  .status-cancelled,
  .status-canceled {
    color: var(--md-sys-color-outline);
  }

  .status-failed,
  .status-error {
    color: var(--md-sys-color-error);
  }

  .job-bytes {
    font-size: 11px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .job-error-msg {
    font-size: 11px;
    color: var(--md-sys-color-error);
    margin-top: 2px;
  }

  .job-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .action-btn {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
    border: none;
    border-radius: var(--md-sys-shape-small);
    padding: 4px 10px;
    font-size: 11px;
    font-weight: 500;
    cursor: pointer;
    transition: opacity 150ms ease;
  }

  .action-btn.primary {
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
  }

  .action-btn.outlined {
    background: transparent;
    border: 1px solid var(--md-sys-color-outline-variant);
    color: var(--md-sys-color-on-surface-variant);
  }

  .action-btn.outlined:hover {
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface);
  }

  .action-btn.danger {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  .action-btn:hover {
    opacity: 0.85;
  }

  /* Material 3 Transfer Progress Bar Styles */
  .drawer-header-progress {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 3px;
    background: var(--md-sys-color-surface-container-highest);
    overflow: hidden;
  }

  .drawer-header-progress-fill {
    height: 100%;
    background: var(--md-sys-color-primary);
    border-radius: 1.5px;
    transition: width 250ms ease;
  }

  .drawer-header-progress-fill.indeterminate {
    width: 35% !important;
    animation: drawer-progress-anim 1.5s infinite linear;
  }

  @keyframes drawer-progress-anim {
    0% { transform: translateX(-100%); }
    100% { transform: translateX(350%); }
  }

  .mini-pct {
    font-size: 11px;
    font-weight: 700;
    color: var(--md-sys-color-primary);
    background: var(--md-sys-color-primary-container);
    padding: 1px 6px;
    border-radius: 6px;
  }

  .job-progress-wrap {
    width: 100%;
    padding: 2px 0;
  }

  .job-progress-track {
    width: 100%;
    height: 4px;
    background: var(--md-sys-color-surface-container-highest);
    border-radius: 2px;
    overflow: hidden;
    position: relative;
  }

  .job-progress-fill {
    height: 100%;
    background: var(--md-sys-color-primary);
    border-radius: 2px;
    transition: width 200ms ease;
  }

  .job-progress-fill.indeterminate {
    width: 40% !important;
    animation: drawer-progress-anim 1.5s infinite linear;
  }

  .job-progress-fraction {
    font-size: 11px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .job-progress-fraction strong {
    color: var(--md-sys-color-primary);
  }
</style>
