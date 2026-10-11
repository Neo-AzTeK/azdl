/**
 * Transfer and Download Size Estimation Utilities
 */

/**
 * Parses human-readable size strings (e.g. "20 GB", "500 MB", "1.5 TB", "30 GB available space")
 * into byte values. Returns null if invalid or unparseable.
 */
export function parseSizeToBytes(str) {
  if (typeof str === 'number' && Number.isFinite(str) && str > 0) {
    return Math.round(str);
  }
  if (!str || typeof str !== 'string') return null;

  const match = str.trim().match(/([\d.]+)\s*([KMGT]?B)/i);
  if (!match) return null;

  const val = parseFloat(match[1]);
  if (isNaN(val) || val <= 0) return null;

  const unit = match[2].toUpperCase();
  if (unit === 'TB') return Math.round(val * 1024 * 1024 * 1024 * 1024);
  if (unit === 'GB') return Math.round(val * 1024 * 1024 * 1024);
  if (unit === 'MB') return Math.round(val * 1024 * 1024);
  if (unit === 'KB') return Math.round(val * 1024);
  if (unit === 'B') return Math.round(val);
  return Math.round(val * 1024 * 1024);
}

/**
 * Estimates the total byte size for a download or installation job.
 * Checks direct job metrics, reported metadata, and fallback game properties.
 */
export function estimateJobTotalBytes(job, fallbackGame = null) {
  if (!job) return 0;

  // 1. Direct explicit total bytes
  const directTotal = job.total_bytes || job.total;
  if (typeof directTotal === 'number' && directTotal > 0) {
    return directTotal;
  }

  // 2. Explicit estimated bytes
  if (typeof job.estimated_bytes === 'number' && job.estimated_bytes > 0) {
    return job.estimated_bytes;
  }

  // 3. Size string on job.install or job
  const jobSizeStr = job.install?.file_size || job.file_size;
  const fromJobStr = parseSizeToBytes(jobSizeStr);
  if (fromJobStr && fromJobStr > 0) {
    return fromJobStr;
  }

  // 4. Fallback game metadata if available
  if (fallbackGame) {
    if (typeof fallbackGame.size_bytes === 'number' && fallbackGame.size_bytes > 0) {
      return fallbackGame.size_bytes;
    }
    const gameSizeStr = fallbackGame.file_size || fallbackGame.storage_size;
    const fromGameStr = parseSizeToBytes(gameSizeStr);
    if (fromGameStr && fromGameStr > 0) {
      return fromGameStr;
    }
  }

  // 5. Dynamic fallback if bytes have been downloaded but total is unknown
  const downloaded = job.bytes || job.progress_bytes || 0;
  if (downloaded > 0) {
    return Math.max(1024 * 1024 * 100, Math.round(downloaded * 1.2));
  }

  return 0;
}

/**
 * Resolves effective total bytes for a job, prioritizing explicit totals over estimated map values.
 */
export function getEffectiveJobTotal(job, fallbackGame = null, dynamicEstimatedMap = null) {
  if (!job) return 0;
  if (job.total_bytes && job.total_bytes > 0) return job.total_bytes;
  if (job.total && job.total > 0) return job.total;

  if (dynamicEstimatedMap && job.id && dynamicEstimatedMap[job.id]) {
    return dynamicEstimatedMap[job.id];
  }

  return estimateJobTotalBytes(job, fallbackGame);
}

/**
 * Calculates a reliable progress percentage (0-100) for a job.
 * Ensures an active job with bytes downloaded shows meaningful advancement.
 */
export function calculateJobProgressPercent(job, effectiveTotal = 0) {
  if (!job) return 0;

  const status = (job.status || '').toLowerCase();
  if (['complete', 'completed', 'finished'].includes(status)) {
    return 100;
  }

  if (job.progress != null && !isNaN(job.progress) && job.progress > 0) {
    return Math.min(100, Math.max(0, Math.round(job.progress)));
  }

  const downloaded = job.bytes || job.progress_bytes || 0;
  const total = effectiveTotal || job.total_bytes || job.total || 0;

  if (total > 0 && downloaded > 0) {
    const pct = Math.round((downloaded / total) * 100);
    return Math.min(99, Math.max(1, pct));
  }

  if (downloaded > 0) {
    return 1;
  }

  return 0;
}
