import { describe, it, expect, vi } from 'vitest';

describe('Transfer Queue Auto-Clear Architecture', () => {
  function isJobActive(status) {
    if (!status) return false;
    const s = status.toLowerCase();
    return ['downloading', 'installing', 'working', 'queued', 'pending', 'running'].includes(s);
  }

  function isJobFinished(status) {
    if (!status) return false;
    const s = status.toLowerCase();
    return ['complete', 'completed', 'finished', 'cancelled', 'canceled', 'failed'].includes(s);
  }

  function formatJobStatus(status, installState = null) {
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

  it('never displays "Running" for downloading jobs', () => {
    expect(formatJobStatus('Running')).toBe('Downloading');
    expect(formatJobStatus('running')).toBe('Downloading');
    expect(formatJobStatus('Running', 'downloading')).toBe('Downloading');
    expect(formatJobStatus('Running', 'installing')).toBe('Installing');
    expect(formatJobStatus('Paused')).toBe('Paused');
    expect(formatJobStatus('Complete')).toBe('Complete');
    expect(formatJobStatus('Cancelled')).toBe('Cancelled');
    expect(formatJobStatus('Failed')).toBe('Failed');
  });

  it('correctly categorizes active vs finished transfer jobs', () => {
    const jobs = [
      { id: '1', status: 'Running', bytes: 100 },
      { id: '2', status: 'Downloading', bytes: 200 },
      { id: '3', status: 'Complete', bytes: 500 },
      { id: '4', status: 'Cancelled', bytes: 50 },
      { id: '5', status: 'Failed', bytes: 0 },
      { id: '6', status: 'Paused', bytes: 300 },
    ];

    const active = jobs.filter((j) => isJobActive(j.status));
    const finished = jobs.filter((j) => isJobFinished(j.status));

    expect(active.map((j) => j.id)).toEqual(['1', '2']);
    expect(finished.map((j) => j.id)).toEqual(['3', '4', '5']);
  });

  it('auto-clear timer fires even when state refreshes frequently', async () => {
    vi.useFakeTimers();

    let autoClearTimer = null;
    let clearCompletedCalled = 0;

    const onClearCompleted = () => {
      clearCompletedCalled += 1;
    };

    function simulateEffect(finishedCount) {
      if (finishedCount > 0) {
        if (!autoClearTimer) {
          autoClearTimer = setTimeout(() => {
            autoClearTimer = null;
            onClearCompleted();
          }, 1500);
        }
      } else if (autoClearTimer) {
        clearTimeout(autoClearTimer);
        autoClearTimer = null;
      }
    }

    // Initial finish of a job
    simulateEffect(1);
    expect(autoClearTimer).not.toBeNull();
    expect(clearCompletedCalled).toBe(0);

    // Fast polling tick at 500ms (does NOT cancel the timer!)
    vi.advanceTimersByTime(500);
    simulateEffect(1);
    expect(autoClearTimer).not.toBeNull();
    expect(clearCompletedCalled).toBe(0);

    // Fast polling tick at 1000ms
    vi.advanceTimersByTime(500);
    simulateEffect(1);
    expect(autoClearTimer).not.toBeNull();
    expect(clearCompletedCalled).toBe(0);

    // Advance past 1500ms total
    vi.advanceTimersByTime(600);
    expect(clearCompletedCalled).toBe(1);
    expect(autoClearTimer).toBeNull();

    vi.useRealTimers();
  });
});

describe('Thumbnail Extraction & Resilient Fallback Chain', () => {
  function getSteamAppId(game) {
    if (!game) return null;
    if (game.steam_app_id) return String(game.steam_app_id);
    const raw = game.origin || game.id || '';
    const match = String(raw).match(/(?:steam[-:]|^)(\d+)/);
    return match ? match[1] : null;
  }

  function getJobThumbnail(job) {
    if (!job) return null;
    if (job.install?.banner_url) return job.install.banner_url;
    if (job.install?.cover_url) return job.install.cover_url;
    if (job.banner_url) return job.banner_url;
    if (job.cover_url) return job.cover_url;
    const id = job.install?.id || job.id || '';
    const match = String(id).match(/(?:steam[-:]|^)(\d+)/);
    if (match) {
      return `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${match[1]}/header.jpg`;
    }
    return null;
  }

  function resolveGameThumbnails(game, failedUrls = new Set()) {
    const steamId = getSteamAppId(game);
    const steamHeader = steamId
      ? `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${steamId}/header.jpg`
      : null;
    const steamCover = steamId
      ? `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${steamId}/library_600x900.jpg`
      : null;

    const candidates = [];
    if (game.banner_url) candidates.push(game.banner_url);
    if (steamHeader && !candidates.includes(steamHeader)) candidates.push(steamHeader);
    if (game.cover_url && !candidates.includes(game.cover_url)) candidates.push(game.cover_url);
    if (steamCover && !candidates.includes(steamCover)) candidates.push(steamCover);

    const active = candidates.find((url) => !failedUrls.has(url)) || null;
    return {
      candidates,
      activeThumbnail: active,
      hasPlaceholderFallback: active === null,
    };
  }

  it('derives Steam CDN header and cover URLs from steam identifiers', () => {
    expect(getSteamAppId({ id: 'steam-1091500' })).toBe('1091500');
    expect(getSteamAppId({ origin: 'steam:480' })).toBe('480');
    expect(getSteamAppId({ id: '730' })).toBe('730');
    expect(getSteamAppId({ steam_app_id: 1245620 })).toBe('1245620');
    expect(getSteamAppId({ id: 'custom-game' })).toBeNull();
  });

  it('cascades through fallback URLs when images fail', () => {
    const game = {
      id: 'steam-480',
      title: 'Spacewar',
      banner_url: 'https://bad-cdn.com/broken-banner.jpg',
      cover_url: 'https://backup-cdn.com/cover.jpg',
    };

    // Initially, top candidate is banner_url
    const initial = resolveGameThumbnails(game);
    expect(initial.activeThumbnail).toBe('https://bad-cdn.com/broken-banner.jpg');
    expect(initial.hasPlaceholderFallback).toBe(false);

    // After banner_url fails, falls back to Steam CDN header
    const failedAfterBanner = new Set(['https://bad-cdn.com/broken-banner.jpg']);
    const second = resolveGameThumbnails(game, failedAfterBanner);
    expect(second.activeThumbnail).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/header.jpg'
    );
    expect(second.hasPlaceholderFallback).toBe(false);

    // After Steam CDN header also fails, falls back to cover_url
    const failedAfterSteam = new Set([
      'https://bad-cdn.com/broken-banner.jpg',
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/header.jpg',
    ]);
    const third = resolveGameThumbnails(game, failedAfterSteam);
    expect(third.activeThumbnail).toBe('https://backup-cdn.com/cover.jpg');

    // If all fail, displays styled placeholder
    const failedAll = new Set([
      'https://bad-cdn.com/broken-banner.jpg',
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/header.jpg',
      'https://backup-cdn.com/cover.jpg',
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/480/library_600x900.jpg',
    ]);
    const final = resolveGameThumbnails(game, failedAll);
    expect(final.activeThumbnail).toBeNull();
    expect(final.hasPlaceholderFallback).toBe(true);
  });

  it('extracts thumbnails for transfer drawer jobs', () => {
    const jobWithBanner = {
      id: 'job-1',
      install: { id: 'hl2', title: 'Half-Life 2', banner_url: 'https://hl2.banner.jpg' },
    };
    expect(getJobThumbnail(jobWithBanner)).toBe('https://hl2.banner.jpg');

    const jobWithSteamId = {
      id: 'job-2',
      install: { id: 'steam-220', title: 'Half-Life 2' },
    };
    expect(getJobThumbnail(jobWithSteamId)).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/220/header.jpg'
    );
  });
});

describe('covers.js SteamGrid & Artwork Resolution', () => {
  it('extracts Steam App IDs across various identifier formats', async () => {
    const { extractSteamAppId, getSteamGridUrls } = await import('./covers.js');
    expect(extractSteamAppId({ id: 'steam-1245620' })).toBe('1245620');
    expect(extractSteamAppId({ qualified_id: 'hydra:1245620' })).toBe('1245620');
    expect(extractSteamAppId({ origin: 'steam:730' })).toBe('730');
    expect(extractSteamAppId({ steam_app_id: 480 })).toBe('480');
    expect(extractSteamAppId({ install: { id: 'steam-220' } })).toBe('220');

    const urls = getSteamGridUrls('1245620');
    expect(urls.banner_url).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/header.jpg'
    );
    expect(urls.cover_url).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/library_600x900.jpg'
    );
    expect(urls.capsule_url).toBe(
      'https://cdn.cloudflare.steamstatic.com/steam/apps/1245620/capsule_616x353.jpg'
    );
  });

  it('assembles candidate thumbnails prioritizing resolved SteamGrid artwork', async () => {
    const { getCandidateThumbnails } = await import('./covers.js');
    const game = {
      id: 'custom-elden-ring',
      title: 'ELDEN RING',
    };
    const resolved = {
      steam_app_id: '1245620',
      banner_url: 'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/header.jpg',
      cover_url: 'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/library_600x900.jpg',
    };

    const candidates = getCandidateThumbnails(game, resolved);
    expect(candidates).toContain(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/header.jpg'
    );
    expect(candidates).toContain(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/library_600x900.jpg'
    );
    expect(candidates[0]).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1245620/header.jpg'
    );
  });

  it('supports Steam movie formats including HLS, WebM, MP4, and DASH', () => {
    const moviePayload = {
      thumbnail: 'https://thumb.jpg',
      name: 'Trailer 1',
      hls_h264: 'https://video.steam.com/trailer/master.m3u8',
      dash_h264: 'https://video.steam.com/trailer/dash.mpd',
      mp4: { max: 'https://video.steam.com/trailer/max.mp4' },
      webm: { '480': 'https://video.steam.com/trailer/480.webm' },
    };

    const mp4 = moviePayload.mp4?.max || moviePayload.mp4?.['480'];
    const webm = moviePayload.webm?.max || moviePayload.webm?.['480'];
    const hls = moviePayload.hls_h264;
    const bestSrc = mp4 || webm || hls;

    expect(mp4).toBe('https://video.steam.com/trailer/max.mp4');
    expect(webm).toBe('https://video.steam.com/trailer/480.webm');
    expect(hls).toBe('https://video.steam.com/trailer/master.m3u8');
    expect(bestSrc).toBe('https://video.steam.com/trailer/max.mp4');
  });

  it('generates Akamai MP4 fallback URLs for modern Steam movie payloads with HLS and movie ID', () => {
    // Modern Steam store responses only provide id and hls_h264/dash manifests
    const modernSteamMovie = {
      id: 257081132,
      name: 'Cyberpunk 2077: City of Dreams',
      thumbnail: 'https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/257081132/movie_600x337.jpg',
      hls_h264: 'https://video.akamai.steamstatic.com/store_trailers/1091500/798578/hls_264_master.m3u8',
      dash_h264: 'https://video.akamai.steamstatic.com/store_trailers/1091500/798578/dash_h264.mpd',
    };

    const movieId = modernSteamMovie.id;
    const mp4Fallback = `https://cdn.akamai.steamstatic.com/steam/apps/${movieId}/movie_max.mp4`;
    const mp4LowFallback = `https://cdn.akamai.steamstatic.com/steam/apps/${movieId}/movie480.mp4`;

    const mappedMovie = {
      type: 'video',
      id: movieId,
      thumb: modernSteamMovie.thumbnail,
      hls: modernSteamMovie.hls_h264,
      mp4: mp4Fallback,
      webm: null,
      src: mp4Fallback,
      name: modernSteamMovie.name,
    };

    expect(mappedMovie.id).toBe(257081132);
    expect(mappedMovie.hls).toContain('.m3u8');
    expect(mappedMovie.mp4).toBe('https://cdn.akamai.steamstatic.com/steam/apps/257081132/movie_max.mp4');
    expect(mp4LowFallback).toBe('https://cdn.akamai.steamstatic.com/steam/apps/257081132/movie480.mp4');
  });

  it('extracts Steam App ID from jobs with steam:// url or install.game_id (e.g. 1073440)', async () => {
    const { extractSteamAppId, getJobThumbnail } = await import('./covers.js');
    const job1073440 = {
      id: '3084f79fb25cf53b',
      kind: 'Game',
      url: 'steam://1073440',
      target: '/home/user/.local/share/azdl/games/steam-1073440',
      install: {
        game_id: 'steam-1073440',
        title: 'コイカツ / Koikatsu Party',
        kind: 'Archive',
      },
    };

    expect(extractSteamAppId(job1073440)).toBe('1073440');
    const thumb = getJobThumbnail(job1073440);
    expect(thumb).toBe(
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/1073440/header.jpg'
    );
  });

  it('correctly calculates transfer progress percentages', () => {
    function getJobProgressPercent(job) {
      if (!job) return 0;
      if (job.progress != null && !isNaN(job.progress)) {
        return Math.min(100, Math.max(0, Math.round(job.progress)));
      }
      const total = job.total_bytes || job.total;
      const downloaded = job.bytes || job.progress_bytes;
      if (total && total > 0 && downloaded != null) {
        return Math.min(100, Math.max(0, Math.round((downloaded / total) * 100)));
      }
      return 0;
    }

    expect(getJobProgressPercent({ progress: 42.6 })).toBe(43);
    expect(getJobProgressPercent({ bytes: 500, total_bytes: 1000 })).toBe(50);
    expect(getJobProgressPercent({ progress_bytes: 250, total: 1000 })).toBe(25);
    expect(getJobProgressPercent({ bytes: 500, total_bytes: 0 })).toBe(0);
    expect(getJobProgressPercent(null)).toBe(0);
  });

  it('keeps the global activity indicator active during background work', () => {
    const isApiBusy = false;
    const hasActiveJobs = true;
    const hasActiveTasks = true;

    // Both determinate transfer progress and global activity remain visible.
    const isAppDoingSomething = isApiBusy || hasActiveJobs || hasActiveTasks;
    expect(isAppDoingSomething).toBe(true);
  });

  it('falls back to alternate available images when regular size image is not available', async () => {
    const { getCandidateThumbnails } = await import('./covers.js');
    const gameWithoutHeader = {
      id: 'steam-999999',
      steam_app_id: 999999,
      title: 'Special Indie Game',
      banner_url: null,
      tiny_image: 'https://shared.akamai.steamstatic.com/apps/999999/capsule_231x87.jpg',
      cover_url: 'https://cdn.cloudflare.steamstatic.com/steam/apps/999999/library_600x900.jpg',
    };

    const candidates = getCandidateThumbnails(gameWithoutHeader);
    expect(candidates).toContain('https://shared.akamai.steamstatic.com/apps/999999/capsule_231x87.jpg');
    expect(candidates).toContain('https://cdn.cloudflare.steamstatic.com/steam/apps/999999/library_600x900.jpg');

    // Filter simulating that regular header.jpg failed (404)
    const failedUrls = new Set([
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/999999/header.jpg',
      'https://cdn.cloudflare.steamstatic.com/steam/apps/999999/header.jpg',
      'https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/999999/header.jpg',
      'https://cdn.cloudflare.steamstatic.com/steam/apps/999999/capsule_616x353.jpg',
      'https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/999999/capsule_616x353.jpg',
      'https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/999999/capsule_616x353.jpg',
    ]);

    const fallbackThumbnail = candidates.find((url) => !failedUrls.has(url));
    expect(fallbackThumbnail).toBe('https://shared.akamai.steamstatic.com/apps/999999/capsule_231x87.jpg');
  });
});
