<script>
  import { onMount, onDestroy, tick } from 'svelte';
  import { Button, Tabs, FAB, LinearProgress, LinearProgressEstimate, Switch } from 'm3-svelte';
  import SectionCard from '../components/SectionCard.svelte';
  import { dlcTransfer, dlcTransferActive, dlcTransferStatus, dlcTransferInstalling, isDlcTransferJob, dlcProgress, filterDlc } from '../dlc.js';
  import iconPlay from '@ktibow/iconset-material-symbols/play-arrow';
  import iconStop from '@ktibow/iconset-material-symbols/stop';
  import iconDownload from '@ktibow/iconset-material-symbols/download';
  import {
    launchGame,
    stopGame,
    runAction,
    diagnoseGame,
    discoverExecutables,
    scanSetup,
    getSteamDetails,
    getGameDlc,
    moveGameToDrive,
    openGameFolder,
  } from '../api.js';
  import { notify, notifyError } from '../notifications.js';
  import Modal from '../components/Modal.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import {
    getResolvedArtwork,
    fetchArtwork,
    normalizeQuery,
    subscribeArtwork,
  } from '../covers.js';
  import {
    parseSizeToBytes,
    estimateJobTotalBytes,
    calculateJobProgressPercent,
  } from '../transfers.js';
  import { getSteamStorageSize } from '../steam_storage.js';

  let {
    game,
    library = [],
    plugins = [],
    settings = null,
    runners = [],
    jobs = [],
    tasks = [],
    dlcRevision = 0,
    fromTab = 'library',
    onBack,
    onLaunchGame = null,
    onStopGame = null,
    onOpenPlugins = null,
    onRefresh = null,
    onShowToast = null,
  } = $props();

  // Determine whether this game is installed in the library
  let installedRecord = $derived.by(() => {
    if (!game) return null;
    const gId = (game.id || '').toLowerCase();
    const qId = (game.qualified_id || '').toLowerCase();
    const gTitle = (game.title || '').trim().toLowerCase();

    return (
      library.find(
        (libGame) =>
          (libGame.id && libGame.id.toLowerCase() === gId) ||
          (libGame.id && qId && libGame.id.toLowerCase() === qId) ||
          (libGame.steam_app_id && game.steam_app_id && Number(libGame.steam_app_id) === Number(game.steam_app_id)) ||
          (libGame.title && libGame.title.trim().toLowerCase() === gTitle)
      ) || (game.install_dir ? game : null)
    );
  });

  let isInstalled = $derived(!!installedRecord);
  let installedGameId = $derived(installedRecord?.id);
  let isRunning = $derived(!!installedRecord?.is_running);
  let dlcInfo = $state({ available: [], installed: [] });
  let dlcPending = $state('');
  let dlcLoading = $state(false);
  let dlcLoadFailed = $state(false);
  let dlcQuery = $state('');
  let dlcFilter = $state('all');
  let dlcRequest = 0;
  let visibleDlc = $derived(filterDlc(dlcInfo.available, dlcQuery, dlcFilter));
  let dlcToRemove = $state(null);
  let dlcToRecover = $state(null);
  let hasInterruptedDlc = $derived((dlcInfo.interrupted || []).length > 0);
  let activeDlcIds = $derived(new Set((tasks || [])
    .filter((task) => task.status === 'running' && task.id?.startsWith(`dlc-${installedRecord?.id}-`))
    .map((task) => task.id.slice(`dlc-${installedRecord.id}-`.length))));
  let previousDlcTasks = new Set();
  let dlcContext = '';
  let displayedSelection = '';

  async function refreshDlc(id) {
    const request = ++dlcRequest;
    dlcLoading = true;
    dlcLoadFailed = false;
    const selectedId = game?.selected_dlc_id;
    try {
      const candidate = Number(selectedId);
      const info = await getGameDlc(id, Number.isInteger(candidate) && candidate > 0 ? candidate : null);
      if (request === dlcRequest && installedRecord?.id === id && game?.selected_dlc_id === selectedId) {
        dlcInfo = info;
        if (info.metadata_error) notify.warning(info.metadata_error);
      }
    } catch (error) {
      if (request === dlcRequest && installedRecord?.id === id && game?.selected_dlc_id === selectedId) {
        dlcLoadFailed = true;
        notifyError(error);
      }
    } finally {
      if (request === dlcRequest) dlcLoading = false;
    }
  }

  $effect(() => {
    void installedGameId;
    dlcQuery = '';
    dlcFilter = 'all';
  });

  $effect(() => {
    const id = installedGameId;
    void dlcRevision;
    const context = `${id || ''}:${game?.selected_dlc_id || ''}`;
    if (context !== dlcContext) {
      dlcContext = context;
      dlcInfo = { available: [], installed: [] };
    }
    if (id) refreshDlc(id);
    else {
      dlcRequest++;
      dlcLoading = false;
    }
  });

  $effect(() => {
    const id = installedRecord?.id;
    const current = new Set((tasks || [])
      .filter((task) => task.status === 'running' && task.id?.startsWith(`dlc-${id}-`))
      .map((task) => task.id));
    if (id && [...previousDlcTasks].some((taskId) => !current.has(taskId))) refreshDlc(id);
    previousDlcTasks = current;
  });

  $effect(() => {
    const selection = game?.selected_dlc_id ? `${installedGameId}:${game.selected_dlc_id}` : '';
    if (!selection) displayedSelection = '';
    if (selection && selection !== displayedSelection && isInstalled && dlcInfo.available.length > 0) {
      displayedSelection = selection;
      activeDetailTab = 'dlc';
      tick().then(() => (document.getElementById(`dlc-${game.selected_dlc_id}`)
        || document.getElementById('section-dlc'))?.scrollIntoView({ behavior: 'smooth', block: 'center' }));
    }
  });

  async function installDlc(addon, resumeJobId = null) {
    if (!installedRecord || dlcPending) return;
    dlcPending = addon.id;
    try {
      const result = await runAction({ action: 'dlc-install', id: installedRecord.id, dlc_id: addon.id, ...(resumeJobId ? { resume_job_id: resumeJobId } : {}) });
      if (!result?.started && !result?.already_working) {
        await refreshDlc(installedRecord.id);
      }
      onRefresh?.();
    } catch (error) {
      notifyError(error);
    } finally {
      dlcPending = '';
    }
  }

  async function cancelDlc(transfer) {
    dlcPending = transfer.id;
    try {
      await runAction({ action: 'job-cancel', id: transfer.id });
      notify.info('Cancellation requested. Waiting for the DLC worker to stop.');
      onRefresh?.();
    } catch (error) {
      notifyError(error);
    } finally {
      dlcPending = '';
    }
  }

  async function removeDlc() {
    if (!installedRecord || !dlcToRemove || dlcPending) return;
    const addon = dlcToRemove;
    dlcPending = addon.id;
    try {
      await runAction({ action: 'dlc-remove', id: installedRecord.id, dlc_id: addon.id });
      notify.success(`${addon.title} removed`);
      await refreshDlc(installedRecord.id);
      onRefresh?.();
    } catch (error) {
      notifyError(error);
    } finally {
      dlcPending = '';
      dlcToRemove = null;
    }
  }

  async function recoverDlc() {
    if (!installedRecord || !dlcToRecover || dlcPending) return;
    const addon = dlcToRecover;
    dlcPending = addon.id;
    try {
      await runAction({ action: 'dlc-recover', id: installedRecord.id, dlc_id: addon.id });
      notify.success(`${addon.title} is ready to install again`);
      await refreshDlc(installedRecord.id);
      onRefresh?.();
    } catch (error) {
      notifyError(error);
    } finally {
      dlcPending = '';
      dlcToRecover = null;
    }
  }

  // Active download/install job for this game
  let activeJob = $derived.by(() => {
    if (!game) return null;
    const gTitle = (game.title || '').trim().toLowerCase();
    const gId = (game.id || '').toLowerCase();
    const qId = (game.qualified_id || '').toLowerCase();

    if (jobs && Array.isArray(jobs) && jobs.length > 0) {
      const match = jobs.find((j) => {
        if (isDlcTransferJob(j)) return false;
        const s = (j.status || '').toLowerCase();
        const instS = (j.install_state || '').toLowerCase();
        const isActive =
          ['running', 'queued', 'downloading', 'working', 'pending'].includes(s) ||
          ['pending', 'installing'].includes(instS);
        if (!isActive) return false;

        if (j.install) {
          if (j.install.id && (j.install.id.toLowerCase() === gId || j.install.id.toLowerCase() === qId)) {
            return true;
          }
          if (j.install.title && j.install.title.trim().toLowerCase() === gTitle) {
            return true;
          }
        }
        if (j.id && (j.id.toLowerCase() === gId || j.id.toLowerCase() === qId)) {
          return true;
        }
        if (j.target) {
          const targetStr = String(j.target).toLowerCase();
          if (gId && targetStr.includes(gId)) return true;
        }
        return false;
      });
      if (match) return match;
    }

    if (tasks && Array.isArray(tasks) && tasks.length > 0) {
      const activeTask = tasks.find((t) => {
        if (t.id?.startsWith('dlc-')) return false;
        const s = (t.status || '').toLowerCase();
        if (s !== 'running' && s !== 'working') return false;
        const tgId = (t.game_id || '').toLowerCase();
        return (
          (tgId && (tgId === gId || tgId === qId)) ||
          (t.id && (t.id === `install-${gId}` || t.id === `install-${qId}`))
        );
      });
      if (activeTask) {
        return {
          id: activeTask.id,
          status: 'Downloading',
          bytes: 0,
          install: { id: gId, title: game.title },
        };
      }
    }

    return null;
  });

  // Extract Steam App ID if applicable
  let steamAppId = $derived.by(() => {
    if (game.steam_app_id) return String(game.steam_app_id);
    if (resolvedArt?.steam_app_id) return String(resolvedArt.steam_app_id);
    const candidates = [
      game.id,
      game.qualified_id,
      game.origin,
      installedRecord?.id,
      installedRecord?.origin,
    ];
    for (const cand of candidates) {
      if (!cand) continue;
      const str = String(cand).trim();
      const match = str.match(/steam[:-](\d+)/i) || str.match(/steamidra:steam[:-](\d+)/i);
      if (match) return match[1];
      if (/^\d{3,9}$/.test(str)) return str;
    }
    return null;
  });

  let resolvedArt = $state(null);

  $effect(() => {
    const titleOrId = game?.title || game?.id;
    if (titleOrId) {
      resolvedArt = getResolvedArtwork(titleOrId);
      if (!game.banner_url && !game.cover_url && !resolvedArt) {
        fetchArtwork(titleOrId).then((art) => {
          if (art) resolvedArt = art;
        });
      }
    }
  });

  $effect(() => {
    const key = normalizeQuery(game?.title || game?.id);
    return subscribeArtwork((_, entry) => (resolvedArt = entry), key);
  });

  let steamHeaderFallback = $derived(
    steamAppId
      ? `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${steamAppId}/header.jpg`
      : resolvedArt?.steam_app_id
        ? `https://shared.cloudflare.steamstatic.com/store_item_assets/steam/apps/${resolvedArt.steam_app_id}/header.jpg`
        : null
  );

  let failedCapsuleUrls = $state(new Set());

  let capsuleCandidates = $derived.by(() => {
    const list = [];
    if (steamData?.header_image) list.push(steamData.header_image);
    if (game.banner_url && !list.includes(game.banner_url)) list.push(game.banner_url);
    if (resolvedArt?.banner_url && !list.includes(resolvedArt.banner_url)) list.push(resolvedArt.banner_url);
    if (steamHeaderFallback && !list.includes(steamHeaderFallback)) list.push(steamHeaderFallback);
    if (game.cover_url && !list.includes(game.cover_url)) list.push(game.cover_url);
    if (resolvedArt?.cover_url && !list.includes(resolvedArt.cover_url)) list.push(resolvedArt.cover_url);
    return list;
  });

  let capsuleImgUrl = $derived.by(() => {
    for (const url of capsuleCandidates) {
      if (!failedCapsuleUrls.has(url)) {
        return url;
      }
    }
    return null;
  });

  function handleCapsuleError(url) {
    if (url) {
      failedCapsuleUrls.add(url);
      failedCapsuleUrls = new Set(failedCapsuleUrls);
    }
  }

  function formatJobStatusLabel(status, installState) {
    if (!status) return 'Downloading game files...';
    const s = status.toLowerCase();
    if (s === 'running' || s === 'downloading') {
      return (installState || '').toLowerCase() === 'installing'
        ? 'Installing game...'
        : 'Downloading game files...';
    }
    if (s === 'installing') return 'Installing game...';
    return status;
  }

  // Steam store metadata (screenshots, videos, detailed description)
  let steamData = $state(null);
  function steamDescriptionText(html) {
    if (typeof DOMParser === 'undefined') return String(html).replace(/<[^>]*>/g, ' ');
    return new DOMParser().parseFromString(html, 'text/html').body.textContent || '';
  }
  let loadingSteam = $state(false);
  let selectedMediaIndex = $state(0);

  // Install configuration (for non-installed games)
  let selectedDriveId = $state('');
  let selectedPluginId = $state('');
  let isInstalling = $state(false);

  $effect(() => {
    if (plugins.length > 0 && !plugins.some((plugin) => plugin.id === selectedPluginId)) {
      const favoriteId = settings?.favorite_plugin_id;
      selectedPluginId = plugins.find((plugin) => plugin.id === favoriteId)?.id
        || (plugins.length === 1 ? plugins[0].id : '');
    }
  });

  // Move game modal state
  let showMoveModal = $state(false);
  let showMoreMenu = $state(false);
  let targetMoveDriveId = $state('');
  let isMoving = $state(false);

  // Installed game config states
  let selectedRunner = $state('auto');
  let selectedExe = $state('');
  let argumentsInput = $state('');
  let envKey = $state('');
  let envVal = $state('');

  // Diagnostic state
  let loadingDiag = $state(false);
  let diagReport = $state(null);

  // Executables scanner
  let discoveringExes = $state(false);
  let exeCandidates = $state([]);

  // Sub-tabs for installed game management
  let activeDetailTab = $state('overview'); // 'overview' | 'patches' | 'config' | 'diagnostics'

  // Optimistic / reactive patch states
  let localSteamStub = $state(null);
  let localDlcPatchApplied = $state(null);
  let localGoldbergHasSettings = $state(null);
  let pendingAction = $state(null);
  let effectiveSteamStub = $derived(localSteamStub || installedRecord?.steamstub);
  let effectiveDlcPatchApplied = $derived(
    localDlcPatchApplied !== null ? localDlcPatchApplied : !!installedRecord?.dlc_patch_applied
  );
  let effectiveGoldbergHasSettings = $derived(
    localGoldbergHasSettings !== null ? localGoldbergHasSettings : !!installedRecord?.goldberg_has_settings
  );

  let hasFix = $derived(!!(installedRecord?.fix && installedRecord.fix !== 'None'));
  let isFixRelevant = $derived(hasFix || (platform === 'linux' && installedRecord?.runner !== 'native'));
  let isSteamStubRelevant = $derived(!!(effectiveSteamStub?.stub_present || effectiveSteamStub?.applied));
  let isEaRelevant = $derived(!!(installedRecord?.ea_heal_available || installedRecord?.ea_heal_applied));
  let isDlcBypassRelevant = $derived(!!(installedRecord?.dlc_patch_available || effectiveDlcPatchApplied));
  let isGoldbergRelevant = $derived(!!(installedRecord?.goldberg_compatible || effectiveGoldbergHasSettings));
  let isPatchRelevant = $derived(!!installedRecord?.patch);
  let isDepsRelevant = $derived(!!(installedRecord?.dependencies && platform === 'linux'));

  $effect(() => {
    // Reset local optimistic override when switching games
    const _ = game?.id;
    localSteamStub = null;
    localDlcPatchApplied = null;
    localGoldbergHasSettings = null;
  });

  $effect(() => {
    if (installedRecord) {
      if (
        localSteamStub &&
        installedRecord.steamstub?.applied === localSteamStub.applied &&
        installedRecord.steamstub?.stub_present === localSteamStub.stub_present
      ) {
        localSteamStub = null;
      }
      if (localDlcPatchApplied !== null && installedRecord.dlc_patch_applied === localDlcPatchApplied) {
        localDlcPatchApplied = null;
      }
      if (localGoldbergHasSettings !== null && installedRecord.goldberg_has_settings === localGoldbergHasSettings) {
        localGoldbergHasSettings = null;
      }
    }
  });

  // Scroll and header translucency state
  let isScrolled = $state(false);

  let displaySize = $derived.by(() => {
    if (game.file_size) return game.file_size;
    if (installedRecord?.size_bytes) return formatBytes(installedRecord.size_bytes);
    return null;
  });

  let displayVersion = $derived.by(() => {
    if (game.version) return game.version;
    if (installedRecord?.version) return installedRecord.version;
    return null;
  });

  function handleScroll(e) {
    isScrolled = e.currentTarget.scrollTop > 10;
  }

  function setBanner(type, message) {
    notify(message, type);
  }

  function formatBytes(bytes) {
    if (!bytes || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return (bytes / Math.pow(1024, i)).toFixed(1) + ' ' + units[i];
  }

  let steamStorageBytes = $state(null);

  $effect(() => {
    if (steamAppId && !steamStorageBytes) {
      getSteamStorageSize(steamAppId).then((size) => {
        if (size) {
          const parsed = parseSizeToBytes(size);
          if (parsed) steamStorageBytes = parsed;
        }
      });
    }
  });

  let effectiveJobTotalBytes = $derived.by(() => {
    if (!activeJob) return 0;
    if (activeJob.total_bytes && activeJob.total_bytes > 0) return activeJob.total_bytes;
    if (activeJob.total && activeJob.total > 0) return activeJob.total;
    if (steamStorageBytes && steamStorageBytes > 0) return steamStorageBytes;
    const fromGame = parseSizeToBytes(game?.file_size || game?.storage_size || steamData?.storage_size);
    if (fromGame && fromGame > 0) return fromGame;
    return estimateJobTotalBytes(activeJob, game);
  });

  function getJobProgressPercent(job) {
    if (!job) return 0;
    return calculateJobProgressPercent(job, effectiveJobTotalBytes);
  }

  // Derive media items (Steam screenshots, movies, hero, fallback)
  let mediaList = $derived.by(() => {
    const list = [];
    if (steamData) {
      // Movies
      if (Array.isArray(steamData.movies)) {
        for (const m of steamData.movies) {
          const movieId = m.id || null;
          const mp4 = typeof m.mp4 === 'string'
            ? m.mp4
            : (m.mp4?.max || m.mp4?.['480'] || (movieId ? `https://cdn.akamai.steamstatic.com/steam/apps/${movieId}/movie_max.mp4` : null));
          const webm = typeof m.webm === 'string'
            ? m.webm
            : (m.webm?.max || m.webm?.['480'] || null);
          const hls = m.hls_h264 || (typeof m.hls === 'string' ? m.hls : null);
          const dash = m.dash_h264 || null;
          const bestSrc = mp4 || hls || webm || dash;
          list.push({
            type: 'video',
            id: movieId,
            thumb: m.thumbnail,
            hls,
            mp4,
            webm,
            src: bestSrc,
            name: m.name,
          });
        }
      }
      // Screenshots
      if (Array.isArray(steamData.screenshots)) {
        for (const s of steamData.screenshots) {
          list.push({
            type: 'image',
            thumb: s.path_thumbnail,
            full: s.path_full,
          });
        }
      }
    }
    // Fallback banner / cover / resolved artwork
    if (list.length === 0) {
      if (game.banner_url) {
        list.push({ type: 'image', thumb: game.banner_url, full: game.banner_url });
      }
      if (resolvedArt?.banner_url && resolvedArt.banner_url !== game.banner_url) {
        list.push({ type: 'image', thumb: resolvedArt.banner_url, full: resolvedArt.banner_url });
      }
      if (game.cover_url) {
        list.push({ type: 'image', thumb: game.cover_url, full: game.cover_url });
      }
      if (resolvedArt?.cover_url && resolvedArt.cover_url !== game.cover_url) {
        list.push({ type: 'image', thumb: resolvedArt.cover_url, full: resolvedArt.cover_url });
      }
    }
    return list;
  });

  let currentMedia = $derived(mediaList[selectedMediaIndex] || mediaList[0] || null);
  let videoPlayerRef = $state(null);
  let hlsInstance = null;
  let videoGeneration = 0;

  async function setupVideoPlayer(videoEl, media) {
    const generation = ++videoGeneration;
    if (!videoEl || !media || media.type !== 'video') return;

    if (hlsInstance) {
      hlsInstance.destroy();
      hlsInstance = null;
    }
    videoEl.pause();
    videoEl.removeAttribute('src');
    videoEl.load();

    const hlsUrl = media.hls;
    const mp4Url = media.mp4;

    let Hls;
    if (hlsUrl) {
      try {
        Hls = (await import('hls.js')).default;
      } catch (error) {
        console.warn('HLS player unavailable; trying direct video:', error);
      }
      if (generation !== videoGeneration) return;
    }

    // 1. Try Hls.js when HLS stream is present and supported
    if (hlsUrl && Hls && typeof Hls.isSupported === 'function' && Hls.isSupported()) {
      const hls = new Hls({
        enableWorker: true,
        lowLatencyMode: false,
      });
      hlsInstance = hls;
      hls.loadSource(hlsUrl);
      hls.attachMedia(videoEl);
      hls.on(Hls.Events.MANIFEST_PARSED, () => {
        videoEl.play().catch(() => {});
      });
      hls.on(Hls.Events.ERROR, (_event, data) => {
        if (data && data.fatal) {
          console.warn('HLS stream error, falling back to MP4/WebM:', data);
          hls.destroy();
          hlsInstance = null;
          const fallback = mp4Url || media.webm;
          if (fallback) {
            videoEl.src = fallback;
            videoEl.load();
            videoEl.play().catch(() => {});
          }
        }
      });
      return;
    }

    // 2. Try native HLS (Safari / macOS WebKit)
    if (hlsUrl && typeof videoEl.canPlayType === 'function' && videoEl.canPlayType('application/vnd.apple.mpegurl')) {
      videoEl.src = hlsUrl;
      videoEl.load();
      videoEl.play().catch(() => {});
      return;
    }

    // 3. Direct MP4/WebM fallback
    const fallbackSrc = mp4Url || media.webm || (media.src !== hlsUrl ? media.src : null);
    if (fallbackSrc) {
      videoEl.src = fallbackSrc;
      videoEl.load();
      videoEl.play().catch(() => {});
    }
  }

  $effect(() => {
    const media = currentMedia;
    const videoEl = videoPlayerRef;
    if (media?.type === 'video' && videoEl) {
      setupVideoPlayer(videoEl, media);
    } else {
      videoGeneration++;
      if (hlsInstance) {
        hlsInstance.destroy();
        hlsInstance = null;
      }
      if (videoEl) {
        videoEl.pause();
        videoEl.removeAttribute('src');
        videoEl.load();
      }
    }
  });

  onDestroy(() => {
    videoGeneration++;
    if (hlsInstance) {
      hlsInstance.destroy();
      hlsInstance = null;
    }
  });

  // Reactive Steam store metadata loader
  let lastFetchedSteamId = $state(null);
  $effect(() => {
    const appId = steamAppId;
    if (appId && appId !== lastFetchedSteamId) {
      lastFetchedSteamId = appId;
      loadingSteam = true;
      getSteamDetails(appId)
        .then((data) => {
          if (data && (data.name || data.screenshots || data.movies)) {
            steamData = data;
          }
        })
        .catch((e) => {
          console.warn('Failed to load Steam store data:', e);
        })
        .finally(() => {
          loadingSteam = false;
        });
    }
  });

  onMount(() => {
    // 1. Initialize default drive for installation
    const drives = settings?.storage_drives || [];
    const defDrive = drives.find((d) => d.is_default) || drives[0];
    selectedDriveId = defDrive?.id || '';

    // 2. Initialize runner and launch options if installed
    if (installedRecord) {
      selectedRunner = installedRecord.runner || 'auto';
      selectedExe = installedRecord.executable || '';
      argumentsInput = (installedRecord.arguments || []).join(' ');
      refreshDiagnostics();
    }
  });

  async function refreshDiagnostics() {
    if (!installedRecord) return;
    loadingDiag = true;
    try {
      diagReport = await diagnoseGame(installedRecord.id);
    } catch (e) {
      console.warn('Diagnostic failed:', e);
    } finally {
      loadingDiag = false;
    }
  }

  async function handleStartInstall() {
    if (isInstalling || activeJob) return;
    const defaultPluginId = selectedPluginId;
    if (game.source_id === 'steam' && (!defaultPluginId || plugins.length === 0)) {
      notifyError(
        {
          code: 'no_plugin_available',
          message: plugins.length ? 'Choose a download plugin for this game.' : 'Install a download plugin from Settings first.',
        },
        {
          onOpenSettings: () => onOpenPlugins?.('plugins'),
        }
      );
      return;
    }
    isInstalling = true;
    const drives = settings?.storage_drives || [];
    const targetDrive = drives.find((d) => d.id === selectedDriveId);

    const installId = game.qualified_id || game.id;

    try {
      const res = await runAction({
        action: 'steam-install',
        id: installId,
        title: game.title,
        banner_url: game.banner_url || game.header_image || game.large_capsule_image || null,
        cover_url: game.cover_url || null,
        plugin_id: game.source_id === 'steam' ? defaultPluginId : undefined,
        location: targetDrive?.id || undefined,
      });

      if (res && res.already_working) {
        setBanner('info', `Installation is already in progress for ${game.title}.`);
      }
      if (onRefresh) onRefresh();
    } catch (err) {
      notifyError(err, {
        onOpenSettings: (cat) => onOpenPlugins?.(cat || 'plugins'),
      });
      setBanner('error', `Installation error: ${err}`);
    } finally {
      isInstalling = false;
    }
  }

  async function handleAction(actionPayload, successMsg) {
    if (!installedRecord || pendingAction) return;
    pendingAction = actionPayload.action;
    try {
      const res = await runAction({ id: installedRecord.id, ...actionPayload });
      if (res?.steamstub) {
        localSteamStub = res.steamstub;
      }
      if (res?.dlc_patch_applied !== undefined) {
        localDlcPatchApplied = res.dlc_patch_applied;
      }
      if (res?.goldberg_has_settings !== undefined) {
        localGoldbergHasSettings = res.goldberg_has_settings;
      }
      setBanner('success', successMsg);
      if (onRefresh) await onRefresh();
      await refreshDiagnostics();
    } catch (err) {
      notifyError(err, {
        onOpenSettings: (cat) => onOpenPlugins?.(cat),
        onConfigureExecutable: () => (showExePicker = true),
      });
      setBanner('error', String(err));
    } finally {
      pendingAction = null;
    }
  }

  async function handlePlay() {
    if (!installedRecord) return;
    if (onLaunchGame) {
      onLaunchGame(installedRecord.id);
    } else {
      await launchGame(installedRecord.id);
      if (onRefresh) onRefresh();
    }
  }

  async function handleStop() {
    if (!installedRecord) return;
    if (onStopGame) {
      onStopGame(installedRecord.id);
    } else {
      await stopGame(installedRecord.id);
      if (onRefresh) onRefresh();
    }
  }

  async function handleMoveGame() {
    if (!installedRecord || !targetMoveDriveId || isMoving) return;
    isMoving = true;
    try {
      await moveGameToDrive(installedRecord.id, targetMoveDriveId);
      setBanner('success', `Successfully moved ${installedRecord.title} to selected drive!`);
      showMoveModal = false;
      if (onRefresh) onRefresh();
    } catch (err) {
      setBanner('error', `Failed to move game: ${err}`);
    } finally {
      isMoving = false;
    }
  }

  async function handleScanExecutables() {
    if (!installedRecord) return;
    discoveringExes = true;
    try {
      exeCandidates = await discoverExecutables(installedRecord.id);
    } catch (e) {
      console.warn(e);
    } finally {
      discoveringExes = false;
    }
  }

  async function handleSetExecutable(exePath) {
    selectedExe = exePath;
    await handleAction({ action: 'executable-set', executable: exePath }, `Executable set to ${exePath}`);
  }

  async function handleSetRunner() {
    await handleAction({ action: 'runner-set', runner: selectedRunner }, `Runner updated to ${selectedRunner}`);
  }

  async function handleSetArguments() {
    const args = argumentsInput.split(' ').map((s) => s.trim()).filter(Boolean);
    await handleAction({ action: 'args-set', arguments: args }, 'Launch arguments updated');
  }

  async function handleAddEnv() {
    if (!envKey.trim()) return;
    await handleAction({ action: 'env-set', key: envKey.trim(), value: envVal.trim() }, `Set environment variable ${envKey}`);
    envKey = '';
    envVal = '';
  }

  async function handleRemoveEnv(k) {
    await handleAction({ action: 'env-remove', key: k }, `Removed variable ${k}`);
  }

  async function handleOpenFolder() {
    if (!installedRecord) return;
    try {
      await openGameFolder(installedRecord.install_dir || installedRecord.id);
    } catch (e) {
      setBanner('error', `Could not open folder: ${e}`);
    }
  }

  let showUninstallConfirm = $state(false);

  function handleUninstall() {
    showUninstallConfirm = true;
  }

  async function executeUninstall() {
    if (!installedRecord) return;
    try {
      await runAction({ action: 'game-remove', id: installedRecord.id });
      notify(`Uninstalled ${installedRecord.title} and removed files`, 'info');
      if (onRefresh) onRefresh();
      onBack();
    } catch (err) {
      notify(`Uninstall failed: ${err}`, 'error');
    }
  }

  function navigateToDetailTab(tabName, targetId = 'game-detail-tabs') {
    showMoreMenu = false;
    activeDetailTab = tabName;
    if (tabName === 'patches' || targetId === 'section-diagnostics') {
      refreshDiagnostics();
    }
    setTimeout(() => {
      const el = document.getElementById(targetId) || document.getElementById('game-detail-tabs');
      if (el) {
        el.scrollIntoView({ behavior: 'smooth', block: 'start' });
      }
    }, 50);
  }
</script>

<div class="game-page-view">
  <!-- Top Navigation Bar (Enlarged Header with M3 Button) -->
  <header class="page-topbar" class:scrolled={isScrolled}>
    <div class="topbar-left">
      <Button
        variant="text"
        size="s"
        iconType="left"
        onclick={onBack}
        aria-label="Back"
      >
        <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor">
          <path d="M20 11H7.83l5.59-5.59L12 4l-8 8 8 8 1.41-1.41L7.83 13H20v-2z" />
        </svg>
        <span>Back</span>
      </Button>

      <h1 class="topbar-title">{steamData?.name || game.title}</h1>
    </div>
  </header>

  <!-- Hero Backdrop Banner with Material 3 Scrim -->
  <div class="hero-backdrop">
    {#if steamData?.header_image || game.banner_url || game.cover_url || steamHeaderFallback}
      <img
        src={steamData?.header_image || game.banner_url || game.cover_url || steamHeaderFallback}
        alt=""
        class="backdrop-img"
        decoding="async"
      />
    {/if}
    <div class="backdrop-gradient"></div>
  </div>

  <div class="page-scroll-body" onscroll={handleScroll}>
    <!-- Action Banner (Mimicking Steam Play / Install Bar) -->
    <section class="game-primary-header">

      <!-- Action Banner (Mimicking Steam Play / Install Bar) -->
      <!-- Unified Steam Action Bar (Play / Install Bar) -->
      <div class="steam-action-bar">
        {#if activeJob}
          <!-- ACTIVE DOWNLOADING STATUS -->
          {@const progress = getJobProgressPercent(activeJob)}
          <div class="active-job-banner">
            <div class="job-meta-header">
              <span class="job-status-label">{formatJobStatusLabel(activeJob.status, activeJob.install_state)}</span>
              <span class="job-bytes-label">
                {formatBytes(activeJob.bytes || 0)}{effectiveJobTotalBytes > 0 ? ` / ${formatBytes(effectiveJobTotalBytes)}` : ''}
              </span>
              {#if progress > 0}
                <span class="job-percent-label">{progress}%</span>
              {/if}
            </div>
            <div class="job-track-bar">
              <div
                class="job-fill-bar"
                class:indeterminate={progress === 0 && effectiveJobTotalBytes === 0}
                style="width: {progress > 0 ? progress + '%' : (effectiveJobTotalBytes > 0 ? '1%' : '30%')}"
              ></div>
            </div>
          </div>
        {:else}
          <!-- ACTION CONTROLS (Unified Left: Play/Stop/Install + Drive, Right: Size/Version + Extra Options if Installed) -->
          <div class="action-bar-left">
            {#if isInstalled}
              {#if installedRecord.is_running}
                <div class="play-bar-fab-wrap play-bar-fab-stop">
                  <FAB
                    size="normal"
                    color="primary"
                    icon={iconStop}
                    text="STOP"
                    onclick={handleStop}
                  />
                </div>
              {:else}
                <div class="play-bar-fab-wrap">
                  <FAB
                    size="normal"
                    color="primary"
                    icon={iconPlay}
                    text="PLAY"
                    disabled={activeDlcIds.size > 0 || !!dlcPending}
                    title={activeDlcIds.size > 0 ? 'Wait for DLC changes to finish before playing' : undefined}
                    onclick={handlePlay}
                  />
                </div>
              {/if}
            {:else}
              <div class="play-bar-fab-wrap">
                <FAB
                  size="normal"
                  color="primary"
                  icon={iconDownload}
                  text={isInstalling ? 'STARTING...' : 'INSTALL GAME'}
                  onclick={handleStartInstall}
                  disabled={isInstalling}
                />
              </div>

              {#if game.source_id === 'steam'}
                <div class="drive-select-container">
                  {#if plugins.length > 0}
                    <label for="game-plugin-select" class="drive-label">Download Plugin:</label>
                    <select id="game-plugin-select" class="md3-select" bind:value={selectedPluginId}>
                      {#if !selectedPluginId}<option value="">Choose a plugin</option>{/if}
                      {#each plugins as plugin}
                        <option value={plugin.id}>{plugin.name}</option>
                      {/each}
                    </select>
                  {:else}
                    <button
                      type="button"
                      class="md3-btn tonal open-plugins-btn"
                      onclick={() => onOpenPlugins && onOpenPlugins()}
                      title="Open Plugins settings to install a download plugin"
                    >
                      <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                        <path d="M20.5 11H19V7c0-1.1-.9-2-2-2h-4V3.5C13 2.12 11.88 1 10.5 1S8 2.12 8 3.5V5H4c-1.1 0-1.99.9-1.99 2v3.8H3.5c1.49 0 2.7 1.21 2.7 2.7s-1.21 2.7-2.7 2.7H2V20c0 1.1.9 2 2 2h3.8v-1.5c0-1.49 1.21-2.7 2.7-2.7 1.49 0 2.7 1.21 2.7 2.7V22H17c1.1 0 2-.9 2-2v-4h1.5c1.38 0 2.5-1.12 2.5-2.5s-1.12-2.5-2.5-2.5z"/>
                      </svg>
                      <span>Plugins</span>
                    </button>
                  {/if}
                </div>
              {/if}

              <!-- Storage Drive Selector (Only present when not installed) -->
              {#if settings?.storage_drives && settings.storage_drives.length > 0}
                <div class="drive-select-container">
                  <label for="game-drive-select" class="drive-label">Install Location:</label>
                  <select id="game-drive-select" class="md3-select" bind:value={selectedDriveId}>
                    {#each settings.storage_drives as drive}
                      <option value={drive.id}>
                        {drive.label} ({drive.path}) {drive.is_default ? '— Default' : ''}
                      </option>
                    {/each}
                  </select>
                </div>
              {/if}
            {/if}
          </div>

          <div class="action-bar-right">
            {#if displaySize}
              <span class="action-meta-chip">Size: <strong>{displaySize}</strong></span>
            {/if}
            {#if displayVersion}
              <span class="action-meta-chip">Version: <strong>v{displayVersion}</strong></span>
            {/if}
            {#if !isInstalled && game.addons_count > 0}
              <span class="action-meta-chip">Addons: <strong>{game.addons_count}</strong></span>
            {/if}

            <!-- Burger / More Menu dropdown button (Only present when installed) -->
            {#if isInstalled}
              <div class="menu-container">
                <button
                  class="icon-btn tonal"
                  onclick={() => (showMoreMenu = !showMoreMenu)}
                  title="More Options"
                  aria-label="More options"
                >
                  <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
                    <path d="M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"/>
                  </svg>
                </button>

                {#if showMoreMenu}
                  <button type="button" class="menu-scrim" onclick={() => (showMoreMenu = false)} aria-label="Close menu"></button>
                  <div class="md3-dropdown-menu">
                    <button class="menu-item" onclick={() => { showMoreMenu = false; handleOpenFolder(); }}>
                      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M20 6h-8l-2-2H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm0 12H4V8h16v10z"/></svg>
                      <span>Open Installation Folder</span>
                    </button>
                    <button class="menu-item" onclick={() => { showMoreMenu = false; showMoveModal = true; }}>
                      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M10 4H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2h-8l-2-2z"/></svg>
                      <span>Move to Another Drive</span>
                    </button>
                    <button class="menu-item" onclick={() => navigateToDetailTab('config')}>
                      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58c.18-.14.23-.41.12-.61l-1.92-3.32c-.12-.22-.37-.29-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54c-.04-.24-.24-.41-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58c-.18.14-.23.41-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z"/></svg>
                      <span>Launch Options & Config</span>
                    </button>
                    <button class="menu-item" onclick={() => navigateToDetailTab('patches', 'section-patches')}>
                      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M18 8h-1V6c0-2.76-2.24-5-5-5S7 3.24 7 6v2H6c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V10c0-1.1-.9-2-2-2zm-6 9c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2zm3.1-9H8.9V6c0-1.71 1.39-3.1 3.1-3.1 1.71 0 3.1 1.39 3.1 3.1v2z"/></svg>
                      <span>Patches</span>
                    </button>
                    <div class="menu-divider"></div>
                    <button class="menu-item danger" onclick={() => { showMoreMenu = false; handleUninstall(); }}>
                      <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z"/></svg>
                      <span>Uninstall Game</span>
                    </button>
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/if}
      </div>
    </section>


    <!-- Two-Column Steam Store & Library Layout -->
    <div class="steam-content-grid">
      <!-- LEFT COLUMN: Media Showcase & Details -->
      <main class="steam-main-col">
        <!-- Steam Market & Store Media Showcase -->
        {#if mediaList.length > 0 && currentMedia}
          <section class="steam-media-showcase">
            <div class="media-featured-stage">
              {#if currentMedia.type === 'video'}
                <!-- svelte-ignore a11y_media_has_caption -->
                <video
                  bind:this={videoPlayerRef}
                  poster={currentMedia.thumb}
                  controls
                  autoplay
                  muted
                  playsinline
                  class="featured-player"
                ></video>
              {:else}
                <img src={currentMedia.full || currentMedia.thumb} alt="" class="featured-img" loading="lazy" decoding="async" />
              {/if}
            </div>

            <!-- Thumbnail Strip Below -->
            {#if mediaList.length > 1}
              <div class="media-thumb-strip">
                {#each mediaList as item, index}
                  <button
                    class="media-thumb-btn"
                    class:active={selectedMediaIndex === index}
                    onclick={() => (selectedMediaIndex = index)}
                  >
                    <img src={item.thumb} alt="" class="thumb-img" loading="lazy" decoding="async" />
                    {#if item.type === 'video'}
                      <div class="video-play-indicator">▶</div>
                    {/if}
                  </button>
                {/each}
              </div>
            {/if}
          </section>
        {/if}

        <!-- Navigation Tabs for Detailed Settings (when installed) -->
        {#if isInstalled}
          <div id="game-detail-tabs" class="game-tabs-container">
            <Tabs
              bind:tab={activeDetailTab}
              items={[
                { name: 'Overview', value: 'overview' },
                { name: `DLC${dlcInfo.available.length ? ` (${dlcInfo.available.length})` : ''}`, value: 'dlc' },
                { name: 'Patches', value: 'patches' },
                { name: 'Launch & Runner', value: 'config' },
              ]}
            />
          </div>
        {/if}

        <!-- TAB: OVERVIEW / ABOUT THIS GAME -->
        {#if !isInstalled || activeDetailTab === 'overview'}
          <section class="steam-section-card">
            <h3 class="section-heading">About This Game</h3>
            <div class="game-description-body">
              {#if steamData?.detailed_description}
                <p>{steamDescriptionText(steamData.detailed_description)}</p>
              {:else if steamData?.short_description}
                <p>{steamData.short_description}</p>
              {:else if game.description}
                <p>{game.description}</p>
              {:else}
                <p class="empty-desc">No detailed description available for this game.</p>
              {/if}
            </div>
          </section>
        {/if}
          {#if isInstalled && activeDetailTab === 'dlc'}
            <div id="section-dlc" aria-busy={dlcLoading}>
            <SectionCard title="Downloadable Content" badge={`${dlcInfo.installed.length} installed`} description="Check existing files or download additional content without reinstalling the base game.">
              {#snippet headerActions()}
                <button class="md3-btn text" disabled={dlcLoading} onclick={() => refreshDlc(installedRecord.id)}>{dlcLoading ? 'Refreshing…' : 'Refresh'}</button>
              {/snippet}
              <div class="dlc-toolbar">
                <input class="md3-input" type="search" aria-label="Search DLC" placeholder="Search downloadable content" bind:value={dlcQuery} />
                <select class="md3-select" aria-label="Filter DLC" bind:value={dlcFilter}>
                  <option value="all">All content</option>
                  <option value="available">Available to install</option>
                  <option value="installed">Installed</option>
                  <option value="recovery">Needs recovery</option>
                </select>
              </div>
              {#if hasInterruptedDlc}<p class="section-subtext">Recover interrupted content before installing or removing DLC.</p>{/if}
              {#if isRunning}<p class="section-subtext">Stop the game before changing downloadable content.</p>{/if}
              {#if activeDlcIds.size > 0}<p class="section-subtext">Wait for the current DLC operation to finish before changing other content.</p>{/if}
              {#each visibleDlc as addon (addon.id)}
                {@const transfer = dlcTransfer(jobs, installedRecord.id, addon.id)}
                {@const transferStatus = dlcTransferStatus(transfer)}
                {@const progress = dlcProgress(transfer)}
                {@const installing = dlcTransferInstalling(transfer)}
                {@const working = activeDlcIds.has(String(addon.id)) || dlcTransferActive(transfer)}
                {@const otherDlcWorking = activeDlcIds.size > 0 && !activeDlcIds.has(String(addon.id))}
                <div id={`dlc-${addon.id}`} class="dlc-row" class:selected={String(game?.selected_dlc_id) === String(addon.id)}>
                  <span class="dlc-details">
                    <span>{String(game?.selected_dlc_id) === String(addon.id) ? game.selected_dlc_title || addon.title : addon.title}</span>
                    <small aria-live="polite">{working ? installing ? 'Installing files…' : progress !== null ? `Downloading · ${progress}% · ${formatBytes(transfer.bytes ?? transfer.progress_bytes)} / ${formatBytes(transfer.total_bytes)}` : 'Checking files and preparing download…' : addon.interrupted ? 'Recovery required' : addon.installed ? 'Installed' : addon.present_unmanaged ? 'Existing files · not managed by azdl' : transferStatus === 'failed' ? 'Download failed · retry available' : transferStatus === 'interrupted' ? 'Download interrupted · retry available' : ['cancelled', 'canceled'].includes(transferStatus) ? 'Download cancelled' : transferStatus === 'paused' ? 'Download paused' : 'Not installed'}</small>
                    {#if working && progress !== null}<LinearProgress percent={progress} aria-label={`Downloading ${addon.title}`} aria-valuenow={progress} aria-valuemin={0} aria-valuemax={100} />{/if}
                    {#if working && progress === null}<LinearProgressEstimate aria-label={`Preparing ${addon.title}`} />{/if}
                    {#if !addon.installed && !addon.interrupted && addon.reason}
                      <small>{addon.reason}</small>
                    {/if}
                  </span>
                  {#if working && transfer && !installing}
                    <button class="md3-btn text" disabled={!!dlcPending} onclick={() => cancelDlc(transfer)}>Cancel</button>
                  {:else if transferStatus === 'paused'}
                    <button class="md3-btn tonal" disabled={isRunning || hasInterruptedDlc || otherDlcWorking || !!dlcPending || !addon.content_available} onclick={() => installDlc(addon, transfer.id)}>Resume</button>
                  {:else if addon.interrupted}
                    <button class="md3-btn danger" disabled={isRunning || otherDlcWorking || !!dlcPending || working} onclick={() => dlcToRecover = addon}>Recover</button>
                  {:else if addon.installed}
                    <button class="md3-btn danger" disabled={isRunning || hasInterruptedDlc || otherDlcWorking || !!dlcPending || working} title={hasInterruptedDlc ? 'Recover interrupted DLC first' : undefined} onclick={() => dlcToRemove = addon}>Remove</button>
                  {:else}
                    <button class="md3-btn tonal" disabled={isRunning || hasInterruptedDlc || otherDlcWorking || !addon.content_available || !!dlcPending || working} title={hasInterruptedDlc ? 'Recover interrupted DLC first' : undefined} onclick={() => installDlc(addon)}>
                      {working ? 'In progress…' : dlcPending === addon.id ? 'Starting…' : ['failed', 'interrupted'].includes(transferStatus) ? 'Retry' : addon.present_unmanaged ? 'Recheck' : addon.needs_existing_check ? 'Check Files' : addon.needs_provider_check ? 'Check & Install' : addon.content_available ? 'Install' : 'Unavailable'}
                    </button>
                  {/if}
                </div>
              {:else}
                <p class="section-subtext">{dlcLoading ? 'Loading downloadable content…' : dlcLoadFailed ? 'Could not load downloadable content. Use Refresh to try again.' : dlcInfo.available.length ? 'No content matches your search or filter.' : 'No downloadable content is offered for this game.'}</p>
              {/each}
            </SectionCard>
            </div>
          {/if}

        <!-- TAB: PATCHES (INSTALLED) -->
        {#if isInstalled && activeDetailTab === 'patches'}
          <section id="section-patches" class="steam-section-card">
            <h3 class="section-heading">Patches & Runtime Overlays</h3>
            <p class="section-subtext">
              Reversible SteamFix, Online-Fix, and SteamStub DRM stripping overlays.
            </p>

            <div class="tools-grid">
              <!-- 1. Overlay DLLs -->
              <div class="tool-card" class:irrelevant={!isFixRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-1 17.93c-3.95-.49-7-3.85-7-7.93 0-.62.08-1.21.21-1.79L9 15v1c0 1.1.9 2 2 2v1.93zm6.9-2.54c-.26-.81-1-1.39-1.9-1.39h-1v-3c0-.55-.45-1-1-1H8v-2h2c.55 0 1-.45 1-1V7h2c1.1 0 2-.9 2-2v-.41c2.93 1.19 5 4.06 5 7.41 0 2.08-.8 3.97-2.1 5.39z"/>
                    </svg>
                    <h4>Online-Fix / SteamFix Overlay</h4>
                  </div>
                  <label class="switch-container" aria-label="Toggle Online-Fix or SteamFix overlay">
                    <Switch
                      checked={hasFix}
                      disabled={isRunning || !isFixRelevant || pendingAction === 'overlay-toggle'}
                      onchange={() => handleAction({ action: 'overlay-toggle' }, 'Toggled DLL overlay')}
                    />
                  </label>
                </div>
                <p class="tool-desc">
                  Toggle multiplayer fix DLLs and FakeAppId via reversible <code>.arachnel-off</code> renames.
                </p>
                <div class="tool-footer">
                  <button
                    class="md3-btn"
                    class:primary={isFixRelevant && !hasFix}
                    class:tonal={hasFix || !isFixRelevant}
                    disabled={isRunning || !isFixRelevant || pendingAction === 'overlay-toggle'}
                    onclick={() => handleAction({ action: 'overlay-toggle' }, 'Toggled DLL overlay')}
                  >
                    <span>{hasFix ? 'Disable DLL Overlay' : isFixRelevant ? 'Enable DLL Overlay' : 'No Multiplayer Fix Detected'}</span>
                  </button>
                </div>
              </div>

              <!-- 2. Steamless DRM -->
              <div class="tool-card" class:irrelevant={!isSteamStubRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M18 8h-1V6c0-2.76-2.24-5-5-5S7 3.24 7 6v2H6c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V10c0-1.1-.9-2-2-2zm-6 9c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2zm3.1-9H8.9V6c0-1.71 1.39-3.1 3.1-3.1 1.71 0 3.1 1.39 3.1 3.1v2z"/>
                    </svg>
                    <h4>Steamless DRM Stripper</h4>
                  </div>
                  <label class="switch-container" aria-label="Toggle Steamless DRM unpack">
                    <Switch
                      checked={!!effectiveSteamStub?.applied}
                      disabled={isRunning || !isSteamStubRelevant || pendingAction === 'steamstub-unpack' || pendingAction === 'steamstub-rollback'}
                      onchange={() => handleAction(
                        { action: effectiveSteamStub?.applied ? 'steamstub-rollback' : 'steamstub-unpack' },
                        effectiveSteamStub?.applied ? 'Restored original SteamStub executable' : 'SteamStub DRM unpacked'
                      )}
                    />
                  </label>
                </div>
                <p class="tool-desc">
                  Strip SteamStub DRM from PE binaries with automated <code>.steamstub.bak</code> backups.
                </p>
                <div class="tool-footer">
                  {#if effectiveSteamStub?.applied}
                    <button
                      class="md3-btn tonal"
                      disabled={isRunning || pendingAction === 'steamstub-rollback'}
                      onclick={() => handleAction({ action: 'steamstub-rollback' }, 'Restored original SteamStub executable')}
                    >
                      <span>{pendingAction === 'steamstub-rollback' ? 'Restoring...' : 'Rollback Original Binary'}</span>
                    </button>
                  {:else}
                    <button
                      class="md3-btn"
                      class:primary={effectiveSteamStub?.stub_present}
                      class:tonal={!effectiveSteamStub?.stub_present}
                      disabled={isRunning || !effectiveSteamStub?.stub_present || pendingAction === 'steamstub-unpack'}
                      onclick={() => handleAction({ action: 'steamstub-unpack' }, 'SteamStub DRM unpacked')}
                    >
                      <span>{pendingAction === 'steamstub-unpack' ? 'Stripping SteamStub DRM...' : effectiveSteamStub?.stub_present ? 'Strip SteamStub DRM' : 'No SteamStub DRM Detected'}</span>
                    </button>
                  {/if}
                </div>
              </div>

              <!-- 3. DLC License Bypass -->
              <div class="tool-card" class:irrelevant={!isDlcBypassRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z"/>
                    </svg>
                    <h4>DLC License Bypass</h4>
                  </div>
                  <label class="switch-container" aria-label="Toggle DLC license bypass">
                    <Switch
                      checked={effectiveDlcPatchApplied}
                      disabled={isRunning || !isDlcBypassRelevant || pendingAction === 'dlc-license-patch' || pendingAction === 'dlc-license-rollback'}
                      onchange={() => handleAction(
                        { action: effectiveDlcPatchApplied ? 'dlc-license-rollback' : 'dlc-license-patch' },
                        effectiveDlcPatchApplied ? 'DLC license patch rolled back' : 'DLC license patch applied'
                      )}
                    />
                  </label>
                </div>
                <p class="tool-desc">
                  Bypasses Steamworks <code>BIsDlcInstalled</code> entitlement checks in <code>steam_api64.dll</code> and GodotSteam.
                </p>
                <div class="tool-footer">
                  {#if effectiveDlcPatchApplied}
                    <button
                      class="md3-btn tonal"
                      disabled={isRunning || pendingAction === 'dlc-license-rollback'}
                      onclick={() => handleAction({ action: 'dlc-license-rollback' }, 'DLC license patch rolled back')}
                    >
                      <span>Rollback DLC Patch</span>
                    </button>
                  {:else}
                    <button
                      class="md3-btn"
                      class:primary={isDlcBypassRelevant}
                      class:tonal={!isDlcBypassRelevant}
                      disabled={isRunning || !isDlcBypassRelevant || pendingAction === 'dlc-license-patch'}
                      onclick={() => handleAction({ action: 'dlc-license-patch' }, 'DLC license patch applied')}
                    >
                      <span>{isDlcBypassRelevant ? 'Apply DLC License Patch' : 'No DLC Check Target Found'}</span>
                    </button>
                  {/if}
                </div>
              </div>

              <!-- 4. Goldberg Steam Emulator Hook -->
              <div class="tool-card" class:irrelevant={!isGoldbergRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8zm-1-13h2v6h-2zm0 8h2v2h-2z"/>
                    </svg>
                    <h4>Goldberg Emulator Settings</h4>
                  </div>
                  <label class="switch-container" aria-label="Toggle Goldberg emulator settings">
                    <Switch
                      checked={effectiveGoldbergHasSettings}
                      disabled={isRunning || !isGoldbergRelevant || pendingAction === 'goldberg-sync' || pendingAction === 'goldberg-rollback'}
                      onchange={() => handleAction(
                        { action: effectiveGoldbergHasSettings ? 'goldberg-rollback' : 'goldberg-sync' },
                        effectiveGoldbergHasSettings ? 'Goldberg configuration removed' : 'Goldberg DLC configuration generated'
                      )}
                    />
                  </label>
                </div>
                <p class="tool-desc">
                  Auto-provisions <code>steam_settings/DLC.txt</code> and offline AppID entitlements for games using Goldberg emulator.
                </p>
                <div class="tool-footer">
                  {#if effectiveGoldbergHasSettings}
                    <button
                      class="md3-btn tonal"
                      disabled={isRunning || pendingAction === 'goldberg-rollback'}
                      onclick={() => handleAction({ action: 'goldberg-rollback' }, 'Goldberg configuration removed')}
                    >
                      <span>Remove Goldberg Config</span>
                    </button>
                  {:else}
                    <button
                      class="md3-btn"
                      class:primary={isGoldbergRelevant}
                      class:tonal={!isGoldbergRelevant}
                      disabled={isRunning || !isGoldbergRelevant || pendingAction === 'goldberg-sync'}
                      onclick={() => handleAction({ action: 'goldberg-sync' }, 'Goldberg DLC configuration generated')}
                    >
                      <span>{isGoldbergRelevant ? 'Generate Goldberg Config' : 'No Steamworks Binaries Found'}</span>
                    </button>
                  {/if}
                </div>
              </div>

              <!-- 5. EA Activation Patch -->
              <div class="tool-card" class:irrelevant={!isEaRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 17h-2v-2h2v2zm2.07-7.75l-.9.92C13.45 12.9 13 13.5 13 15h-2v-.5c0-1.1.45-2.1 1.17-2.83l1.24-1.26c.37-.36.59-.86.59-1.41 0-1.1-.9-2-2-2s-2 .9-2 2H7c0-2.76 2.24-5 5-5s5 2.24 5 5c0 1.04-.42 1.99-1.07 2.75z"/>
                    </svg>
                    <h4>EA Activation Patch</h4>
                  </div>
                  <label class="switch-container" aria-label="Toggle EA activation patch">
                    <Switch
                      checked={!!installedRecord.ea_heal_applied}
                      disabled={isRunning || !isEaRelevant || pendingAction === 'ea-heal' || pendingAction === 'ea-rollback'}
                      onchange={() => handleAction(
                        { action: installedRecord.ea_heal_applied ? 'ea-rollback' : 'ea-heal' },
                        installedRecord.ea_heal_applied ? 'EA activation restored' : 'EA activation patched'
                      )}
                    />
                  </label>
                </div>
                <p class="tool-desc">
                  Bypasses EA Desktop / Origin activation checks and restores original entitlement binaries.
                </p>
                <div class="tool-footer">
                  {#if installedRecord.ea_heal_applied}
                    <button
                      class="md3-btn tonal"
                      disabled={isRunning || pendingAction === 'ea-rollback'}
                      onclick={() => handleAction({ action: 'ea-rollback' }, 'EA activation restored')}
                    >
                      <span>Rollback EA Patch</span>
                    </button>
                  {:else}
                    <button
                      class="md3-btn"
                      class:primary={isEaRelevant}
                      class:tonal={!isEaRelevant}
                      disabled={isRunning || !isEaRelevant || pendingAction === 'ea-heal'}
                      onclick={() => handleAction({ action: 'ea-heal' }, 'EA activation patched')}
                    >
                      <span>{isEaRelevant ? 'Apply Activation Patch' : 'No EA Activation Binaries Found'}</span>
                    </button>
                  {/if}
                </div>
              </div>

              <!-- 6. Standalone Game Patch -->
              <div class="tool-card" class:irrelevant={!isPatchRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M19 3H5c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm-7 14l-5-5 1.41-1.41L12 14.17l7.59-7.59L21 8l-9 9z"/>
                    </svg>
                    <h4>Standalone Game Patch</h4>
                  </div>
                  <label class="switch-container" aria-label="Toggle standalone game patch">
                    <Switch
                      checked={installedRecord.patch === 'enabled'}
                      disabled={isRunning || !isPatchRelevant || pendingAction === 'patch-toggle'}
                      onchange={() => handleAction({ action: 'patch-toggle' }, 'Toggled game patch')}
                    />
                  </label>
                </div>
                <p class="tool-desc">
                  Toggle standalone game patch files and mod payloads staged in this game directory.
                </p>
                <div class="tool-footer">
                  <button
                    class="md3-btn"
                    class:primary={isPatchRelevant && installedRecord.patch !== 'enabled'}
                    class:tonal={installedRecord.patch === 'enabled' || !isPatchRelevant}
                    disabled={isRunning || !isPatchRelevant || pendingAction === 'patch-toggle'}
                    onclick={() => handleAction({ action: 'patch-toggle' }, 'Toggled game patch')}
                  >
                    <span>{installedRecord.patch === 'enabled' ? 'Disable Patch' : isPatchRelevant ? 'Enable Patch' : 'No Patch Payload Staged'}</span>
                  </button>
                </div>
              </div>

              <!-- 7. Runtime Dependencies -->
              <div class="tool-card" class:irrelevant={!isDepsRelevant}>
                <div class="tool-header">
                  <div class="tool-header-left">
                    <svg class="tool-header-icon" viewBox="0 0 24 24" fill="currentColor">
                      <path d="M4 6h16v12H4zM2 4v16h20V4H2zm3 2h2v2H5V6zm0 3h2v2H5V9zm0 3h2v2H5v-2zm14 3h-2v-2h2v2zm0-3h-2v-2h2v2zm0-3h-2V6h2v2z"/>
                    </svg>
                    <h4>Runtime Dependencies</h4>
                  </div>
                  {#if !isDepsRelevant}
                    <span class="tool-pill inactive">Host OS Native</span>
                  {:else if installedRecord.dependencies?.missing?.length === 0}
                    <span class="tool-pill success">Satisfied</span>
                  {:else}
                    <span class="tool-pill warning">Pending</span>
                  {/if}
                </div>
                <p class="tool-desc">
                  {#if !isDepsRelevant}
                    Native execution environment; DirectX and VC++ runtimes are managed by the host OS.
                  {:else if installedRecord.dependencies?.missing?.length === 0}
                    Visual C++ 2015-2022 and DirectX runtimes are satisfied in this Wine/Proton prefix.
                  {:else}
                    Missing: <code>{installedRecord.dependencies?.missing?.join(', ')}</code>. Automatically provisioned before launch.
                  {/if}
                </p>
                <div class="tool-footer">
                  {#if isDepsRelevant && (!installedRecord.dependencies?.vcredist_x64 || !installedRecord.dependencies?.directx)}
                    <div style="display: flex; gap: 8px; width: 100%;">
                      {#if !installedRecord.dependencies.vcredist_x64}
                        <button
                          class="md3-btn primary small"
                          style="flex: 1;"
                          disabled={isRunning || pendingAction === 'install-vcredist'}
                          onclick={() => handleAction({ action: 'install-vcredist' }, 'Visual C++ 2015-2022 (x64) installed')}
                        >
                          <span>Install VC++</span>
                        </button>
                      {/if}
                      {#if !installedRecord.dependencies.directx}
                        <button
                          class="md3-btn tonal small"
                          style="flex: 1;"
                          disabled={isRunning || pendingAction === 'install-directx'}
                          onclick={() => handleAction({ action: 'install-directx' }, 'DirectX runtimes installed')}
                        >
                          <span>Install DirectX</span>
                        </button>
                      {/if}
                    </div>
                  {:else}
                    <button class="md3-btn tonal" disabled={true}>
                      <span>{isDepsRelevant ? 'Runtimes Satisfied' : 'Host OS Managed'}</span>
                    </button>
                  {/if}
                </div>
              </div>
            </div>
          </section>

          <!-- Merged Environment Diagnostics section -->
          <section id="section-diagnostics" class="steam-section-card" style="margin-top: 16px;">
            <div class="diag-header-row">
              <h3 class="section-heading">Environment Diagnostics & Readiness</h3>
              <button class="md3-btn tonal small" onclick={refreshDiagnostics} disabled={isRunning || loadingDiag}>
                {loadingDiag ? 'Scanning...' : 'Re-run Checks'}
              </button>
            </div>

            {#if loadingDiag}
              <p class="diag-loading">Checking runner binaries, permissions, and prefix...</p>
            {:else if diagReport}
              <div class="diag-checks-list">
                {#if !diagReport.checks || diagReport.checks.length === 0}
                  <div class="diag-check check-success">
                    <span>✔ All system and runner checks passed. System ready for launch.</span>
                  </div>
                {:else}
                  {#each diagReport.checks as check}
                    <div class="diag-check check-{check.severity}">
                      <span class="diag-severity-tag">{check.severity}</span>
                      <span class="diag-message">{check.message}</span>
                    </div>
                  {/each}
                {/if}
              </div>
            {/if}
          </section>
        {/if}

        <!-- TAB: LAUNCH & RUNNER CONFIG (INSTALLED) -->
        {#if isInstalled && activeDetailTab === 'config'}
          <section class="steam-section-card">
            <h3 class="section-heading">Launch Options & Runner Configuration</h3>

            <div class="config-form">
              <!-- Compatibility Runner -->
              <div class="form-row">
                <label for="runner-select" class="form-label">Compatibility Runner:</label>
                <div class="row-input-group">
                  <select id="runner-select" class="md3-select" bind:value={selectedRunner}>
                    {#each runners as r}
                      <option value={r.id}>{r.name}</option>
                    {/each}
                  </select>
                  <button class="md3-btn tonal small" onclick={handleSetRunner}>Save Runner</button>
                </div>
              </div>

              <!-- Primary Executable -->
              <div class="form-row">
                <label for="exe-input" class="form-label">Primary Executable:</label>
                <div class="row-input-group">
                  <input
                    id="exe-input"
                    type="text"
                    class="md3-input"
                    bind:value={selectedExe}
                    placeholder="Relative or absolute path to .exe"
                  />
                  <button class="md3-btn tonal small" onclick={() => handleSetExecutable(selectedExe)}>Save</button>
                  <button class="md3-btn tonal small" onclick={handleScanExecutables} disabled={discoveringExes}>
                    {discoveringExes ? 'Scanning...' : 'Scan Directory'}
                  </button>
                </div>

                {#if exeCandidates.length > 0}
                  <div class="candidates-selector">
                    <span class="candidates-title">Discovered executables:</span>
                    <div class="candidates-chips">
                      {#each exeCandidates as cand}
                        <button
                          class="candidate-chip"
                          onclick={() => handleSetExecutable(cand.executable)}
                        >
                          {cand.executable} (Score: {cand.score})
                        </button>
                      {/each}
                    </div>
                  </div>
                {/if}
              </div>

              <!-- Launch Arguments -->
              <div class="form-row">
                <label for="args-input" class="form-label">Launch Arguments:</label>
                <div class="row-input-group">
                  <input
                    id="args-input"
                    type="text"
                    class="md3-input"
                    bind:value={argumentsInput}
                    placeholder="-windowed -novid %command%"
                  />
                  <button class="md3-btn tonal small" onclick={handleSetArguments}>Save Arguments</button>
                </div>
              </div>

              <!-- Environment Overrides -->
              <div class="form-row">
                <label for="env-key-input" class="form-label">Environment Overrides:</label>
                <div class="env-editor-row">
                  <input id="env-key-input" type="text" class="md3-input" placeholder="Variable (e.g. DXVK_HUD)" bind:value={envKey} />
                  <input type="text" class="md3-input" placeholder="Value (e.g. fps)" bind:value={envVal} />
                  <button class="md3-btn tonal small" onclick={handleAddEnv}>Add</button>
                </div>

                {#if installedRecord.environment && Object.keys(installedRecord.environment).length > 0}
                  <div class="env-table">
                    {#each Object.entries(installedRecord.environment) as [k, v]}
                      <div class="env-item">
                        <span class="env-k">{k}</span>
                        <span class="env-eq">=</span>
                        <span class="env-v">{v}</span>
                        <button class="env-remove-btn" onclick={() => handleRemoveEnv(k)}>✕</button>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>
          </section>
        {/if}
      </main>

      <!-- RIGHT COLUMN: Sidebar Metadata & Fast Controls -->
      <aside class="steam-sidebar-col">
        <!-- Horizontal Capsule Artwork -->
        <div class="steam-capsule-card">
          {#if capsuleImgUrl}
            <img
              src={capsuleImgUrl}
              alt={game.title}
              class="capsule-img"
              loading="lazy"
              decoding="async"
              onerror={() => handleCapsuleError(capsuleImgUrl)}
            />
          {:else}
            <div class="capsule-placeholder">
              <svg viewBox="0 0 24 24" width="32" height="32" fill="currentColor" opacity="0.6">
                <path d="M21 6H3c-1.1 0-2 .9-2 2v8c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm-10 7H8v3H6v-3H3v-2h3V8h2v3h3v2zm4.5 2c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm4-3c-.83 0-1.5-.67-1.5-1.5S18.67 9 19.5 9s1.5.67 1.5 1.5-.67 1.5-1.5 1.5z"/>
              </svg>
              <span>{game.title}</span>
            </div>
          {/if}
        </div>

        <!-- Game Metadata Card -->
        <div class="sidebar-details-card">
          <h4 class="sidebar-title">Game Information</h4>

          <div class="sidebar-data-row">
            <span class="data-label">App ID</span>
            <span class="data-value">{game.id}</span>
          </div>

          {#if steamAppId}
            <div class="sidebar-data-row">
              <span class="data-label">Steam App ID</span>
              <span class="data-value">{steamAppId}</span>
            </div>
          {/if}

          {#if (steamData?.genres && steamData.genres.length > 0) || (Array.isArray(game.genres) && game.genres.length > 0) || game.version}
            <div class="sidebar-data-row tags-data-row">
              <span class="data-label">Genres & Tags</span>
              <div class="sidebar-tags-wrap">
                {#if steamData?.genres}
                  {#each steamData.genres as g}
                    <span class="genre-tag">{g.description}</span>
                  {/each}
                {:else if Array.isArray(game.genres)}
                  {#each game.genres as g}
                    <span class="genre-tag">{g}</span>
                  {/each}
                {/if}
                {#if game.version}
                  <span class="genre-tag version-tag">v{game.version}</span>
                {/if}
              </div>
            </div>
          {/if}

          {#if steamData?.release_date}
            <div class="sidebar-data-row">
              <span class="data-label">Release Date</span>
              <span class="data-value">{steamData.release_date.date}</span>
            </div>
          {/if}

          {#if steamData?.developers}
            <div class="sidebar-data-row">
              <span class="data-label">Developer</span>
              <span class="data-value">{steamData.developers.join(', ')}</span>
            </div>
          {/if}

          {#if isInstalled}
            <div class="sidebar-data-row">
              <span class="data-label">Runner</span>
              <span class="data-value">{installedRecord.runner || 'Auto'}</span>
            </div>
          {/if}

          {#if displaySize}
            <div class="sidebar-data-row">
              <span class="data-label">Size</span>
              <span class="data-value">{displaySize}</span>
            </div>
          {/if}

          {#if displayVersion}
            <div class="sidebar-data-row">
              <span class="data-label">Version</span>
              <span class="data-value">v{displayVersion}</span>
            </div>
          {/if}

          {#if game.source_id}
            <div class="sidebar-data-row">
              <span class="data-label">Catalog Source</span>
              <span class="data-value">{game.source_id}</span>
            </div>
          {/if}
        </div>
      </aside>
    </div>
  </div>

  <!-- Reusable Move Game Modal -->
  <Modal
    bind:open={showMoveModal}
    title="Move Game to Another Drive"
    onClose={() => (showMoveModal = false)}
  >
    <div class="move-modal-body">
      <p class="modal-description">
        Relocate <strong>{installedRecord?.title}</strong> to a different storage drive. All files and prefixes will be transferred safely.
      </p>

      <div class="form-group">
        <label for="move-drive-select" class="form-label">Target Storage Drive:</label>
        <select id="move-drive-select" class="md3-select" bind:value={targetMoveDriveId}>
          <option value="" disabled selected>Select destination drive...</option>
          {#if settings?.storage_drives}
            {#each settings.storage_drives as drive}
              <option value={drive.id}>
                {drive.label} — {drive.path}
              </option>
            {/each}
          {/if}
        </select>
      </div>

      <div class="modal-actions-row">
        <button class="md3-btn tonal" onclick={() => (showMoveModal = false)}>Cancel</button>
        <button
          class="md3-btn primary"
          onclick={handleMoveGame}
          disabled={!targetMoveDriveId || isMoving}
        >
          {isMoving ? 'Moving files...' : 'Move Game Now'}
        </button>
      </div>
    </div>
  </Modal>

  <ConfirmModal
    bind:open={showUninstallConfirm}
    title="Uninstall Game"
    message={`Are you sure you want to completely uninstall and delete ${steamData?.name || game.title}? All installed game files and local data will be permanently removed from disk.`}
    confirmText="Uninstall"
    isDestructive={true}
    onConfirm={executeUninstall}
  />
  <ConfirmModal
    open={!!dlcToRemove}
    title="Remove DLC"
    message={`Remove ${dlcToRemove?.title || 'this DLC'} and its installed files?`}
    confirmText="Remove"
    isDestructive={true}
    onConfirm={removeDlc}
    onCancel={() => dlcToRemove = null}
  />
  <ConfirmModal
    open={!!dlcToRecover}
    title="Recover Interrupted DLC"
    message={`Remove files left by the interrupted ${dlcToRecover?.title || 'DLC'} installation?`}
    confirmText="Recover"
    isDestructive={true}
    onConfirm={recoverDlc}
    onCancel={() => dlcToRecover = null}
  />
</div>

<style>
  .dlc-toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 16px;
  }
  .dlc-toolbar input { flex: 1; min-width: 180px; }
  .dlc-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.75rem 0;
    border-top: 1px solid var(--md-sys-color-outline-variant);
    scroll-margin-block: 5rem;
  }
  .dlc-row.selected {
    background: var(--md-sys-color-surface-container-high);
    border-radius: var(--md-sys-shape-medium);
    padding-inline: 0.75rem;
  }
  .dlc-details {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .dlc-details small {
    color: var(--md-sys-color-on-surface-variant);
  }
  .game-page-view {
    position: relative;
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--md-sys-color-surface, #121212);
    color: var(--md-sys-color-on-surface, #e2e8f0);
  }

  /* Top Navigation Bar */
  .page-topbar {
    position: relative;
    z-index: 10;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 20px 36px;
    min-height: 80px;
    background: transparent;
    border-bottom: 1px solid transparent;
    transition: background-color 0.2s ease, border-color 0.2s ease;
  }

  .page-topbar.scrolled {
    background: var(--md-sys-color-surface-container);
    border-bottom-color: var(--md-sys-color-outline-variant);
  }

  .topbar-left {
    display: flex;
    align-items: center;
    gap: 20px;
    min-width: 0;
  }

  .topbar-title {
    font-size: 1.85rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    margin: 0;
    color: var(--md-sys-color-on-surface);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }


  .catalog-pill {
    background: var(--md-sys-color-tertiary-container);
    color: var(--md-sys-color-on-tertiary-container);
  }

  /* Hero Backdrop */
  .hero-backdrop {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 420px;
    z-index: 1;
    overflow: hidden;
    pointer-events: none;
  }

  .backdrop-img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    object-position: center 25%;
    filter: blur(28px) brightness(0.4);
    transform: scale(1.15);
  }

  .backdrop-gradient {
    position: absolute;
    inset: 0;
    background: linear-gradient(
      to bottom,
      rgba(0, 0, 0, 0.2) 0%,
      rgba(0, 0, 0, 0.65) 50%,
      var(--md-sys-color-surface, #121212) 100%
    );
  }

  /* Scrollable Page Body */
  .page-scroll-body {
    position: relative;
    z-index: 2;
    flex: 1;
    overflow-y: auto;
    padding: 24px 32px 64px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  /* Primary Header & Action Bar */
  .game-primary-header {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .tags-data-row {
    display: flex;
    flex-direction: row;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px;
  }

  .tags-data-row .data-label {
    flex-shrink: 0;
    text-align: left;
    padding-top: 3px;
  }

  .sidebar-tags-wrap {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    align-items: center;
    gap: 6px;
    max-width: 65%;
  }

  .genre-tag {
    font-size: 11px;
    font-weight: 500;
    padding: 4px 10px;
    border-radius: var(--md-sys-shape-small);
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface-variant);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .genre-tag.source-tag {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
    border-color: transparent;
    font-weight: 600;
  }

  .genre-tag.version-tag {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
    border-color: transparent;
  }

  /* Action Bar (Material Design 3 Surface) */
  .steam-action-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 24px;
    border-radius: var(--md-sys-shape-large);
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.25);
  }

  .action-bar-left {
    display: flex;
    align-items: center;
    gap: 20px;
    flex-wrap: wrap;
  }

  .action-bar-right {
    display: flex;
    align-items: center;
    gap: 10px;
    position: relative;
  }

  .play-bar-fab-wrap {
    display: inline-flex;
    align-items: center;
  }

  .play-bar-fab-wrap :global(button) {
    min-width: 140px;
    font-weight: 700;
    letter-spacing: 0.04em;
  }

  .play-bar-fab-stop :global(button) {
    background-color: var(--md-sys-color-error) !important;
    color: var(--md-sys-color-on-error) !important;
  }


  .action-meta-chip {
    font-size: 13px;
    color: var(--md-sys-color-on-surface-variant);
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    padding: 6px 14px;
    border-radius: var(--md-sys-shape-full);
  }

  .action-meta-chip strong {
    color: var(--md-sys-color-on-surface);
  }

  .drive-select-container {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .drive-label {
    font-size: 12px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .meta-highlights {
    display: flex;
    gap: 12px;
  }

  .highlight-chip {
    font-size: 13px;
    padding: 6px 14px;
    border-radius: var(--md-sys-shape-small);
    background: var(--md-sys-color-surface-container);
    color: var(--md-sys-color-on-surface-variant);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  /* Active Job Banner */
  .active-job-banner {
    width: 100%;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .job-meta-header {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 13px;
  }

  .job-pulse-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--md-sys-color-primary);
    box-shadow: 0 0 8px var(--md-sys-color-primary);
    animation: pulse 1.2s infinite;
  }

  .job-status-label {
    font-weight: 600;
    color: var(--md-sys-color-primary);
  }

  .job-bytes-label {
    margin-left: auto;
    color: var(--md-sys-color-on-surface-variant);
  }

  .job-percent-label {
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
  }

  .job-track-bar {
    width: 100%;
    height: 6px;
    border-radius: 3px;
    background: var(--md-sys-color-surface-container-highest);
    overflow: hidden;
  }

  .job-fill-bar {
    height: 100%;
    background: var(--md-sys-color-primary);
    border-radius: 3px;
    transition: width 0.3s ease;
  }

  /* Alert Banner */
  .page-alert {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 18px;
    border-radius: var(--md-sys-shape-medium);
    font-size: 13px;
  }

  .page-alert.info {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .page-alert.success {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .page-alert.error {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .alert-dismiss {
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
  }

  /* Content Grid */
  .steam-content-grid {
    display: grid;
    grid-template-columns: 1fr 340px;
    gap: 28px;
  }

  .steam-main-col {
    display: flex;
    flex-direction: column;
    gap: 24px;
    min-width: 0;
  }

  .steam-sidebar-col {
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  /* Media Showcase */
  .steam-media-showcase {
    display: flex;
    flex-direction: column;
    gap: 12px;
    background: var(--md-sys-color-surface-container);
    padding: 14px;
    border-radius: var(--md-sys-shape-large);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .media-featured-stage {
    position: relative;
    width: 100%;
    aspect-ratio: 16 / 9;
    border-radius: var(--md-sys-shape-medium);
    overflow: hidden;
    background: #000;
  }

  .featured-img,
  .featured-player {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .media-thumb-strip {
    display: flex;
    gap: 8px;
    overflow-x: auto;
    padding-bottom: 4px;
  }

  .media-thumb-btn {
    position: relative;
    width: 110px;
    aspect-ratio: 16 / 9;
    flex-shrink: 0;
    border-radius: var(--md-sys-shape-small);
    overflow: hidden;
    border: 2px solid transparent;
    cursor: pointer;
    background: #000;
    padding: 0;
    transition: transform 0.15s ease, border-color 0.15s ease;
  }

  .media-thumb-btn.active {
    border-color: var(--md-sys-color-primary);
    transform: scale(1.04);
  }

  .thumb-img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .video-play-indicator {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.45);
    color: var(--md-sys-color-on-surface);
    font-size: 14px;
  }

  /* Section Card */
  .steam-section-card {
    background: var(--md-sys-color-surface-container);
    border-radius: var(--md-sys-shape-large);
    border: 1px solid var(--md-sys-color-outline-variant);
    padding: 24px;
    min-width: 0;
    max-width: 100%;
    overflow: hidden;
  }

  .section-heading {
    font-size: 18px;
    font-weight: 700;
    margin: 0 0 12px;
    color: var(--md-sys-color-on-surface);
  }

  .section-subtext {
    font-size: 13px;
    color: var(--md-sys-color-on-surface-variant);
    margin: 0 0 20px;
  }

  .game-description-body {
    font-size: 14px;
    line-height: 1.6;
    color: var(--md-sys-color-on-surface-variant);
    overflow-x: hidden;
    word-break: break-word;
    max-width: 100%;
  }

  .game-description-body :global(img) {
    max-width: 100% !important;
    height: auto !important;
    border-radius: var(--md-sys-shape-small, 8px);
    display: block;
    margin: 12px 0;
  }

  .game-description-body :global(table) {
    max-width: 100% !important;
    overflow-x: auto;
    display: block;
  }

  .game-description-body :global(iframe) {
    max-width: 100% !important;
  }

  /* Section Tabs */
  .game-tabs-container {
    width: 100%;
    margin-bottom: 8px;
    border-radius: var(--md-sys-shape-medium);
    overflow: hidden;
  }

  /* Tools Grid */
  .tools-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    gap: 16px;
  }

  .tool-card {
    background: var(--md-sys-color-surface-container);
    border-radius: var(--md-sys-shape-medium);
    padding: 16px;
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    gap: 12px;
    min-height: 200px;
    border: 1px solid var(--md-sys-color-outline-variant);
    transition: background-color 0.2s ease, border-color 0.2s ease, opacity 0.2s ease;
  }

  .tool-card:hover:not(.irrelevant) {
    background: var(--md-sys-color-surface-container-high);
    border-color: var(--md-sys-color-outline);
  }

  .tool-card.irrelevant {
    opacity: 0.65;
    background: var(--md-sys-color-surface-container-low);
  }

  .tool-card.irrelevant .tool-header-icon {
    color: var(--md-sys-color-on-surface-variant);
  }

  .tool-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }

  .switch-container {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    flex-shrink: 0;
  }

  :global(.switch-container:has(input:disabled)) {
    cursor: not-allowed;
    opacity: 0.38;
    pointer-events: none;
  }

  .tool-header-left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .tool-header-icon {
    width: 20px;
    height: 20px;
    color: var(--md-sys-color-primary);
    flex-shrink: 0;
  }

  .tool-header h4 {
    margin: 0;
    font-size: 14px;
    font-weight: 700;
    color: var(--md-sys-color-on-surface);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tool-pill {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: var(--md-sys-shape-small);
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .tool-pill.active,
  .tool-pill.success {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
  }

  .tool-pill.warning {
    background: var(--md-sys-color-tertiary-container);
    color: var(--md-sys-color-on-tertiary-container);
  }

  .tool-pill.inactive {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface-variant);
  }

  .tool-desc {
    font-size: 12px;
    color: var(--md-sys-color-on-surface-variant);
    line-height: 1.5;
    margin: 0;
    flex: 1;
  }

  .tool-footer {
    display: flex;
    gap: 8px;
    width: 100%;
    margin-top: auto;
  }

  .tool-card .md3-btn {
    width: 100%;
    justify-content: center;
    margin-top: auto;
  }

  /* Config Form */
  .config-form {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }

  .form-row {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .form-label {
    font-size: 13px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface-variant);
  }

  .row-input-group {
    display: flex;
    gap: 10px;
    align-items: center;
  }

  .candidates-selector {
    margin-top: 8px;
    padding: 10px;
    background: var(--md-sys-color-surface-container-high);
    border-radius: var(--md-sys-shape-small);
  }

  .candidates-title {
    font-size: 12px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .candidates-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 6px;
  }

  .candidate-chip {
    padding: 4px 10px;
    border-radius: var(--md-sys-shape-small);
    border: 1px solid var(--md-sys-color-outline-variant);
    background: var(--md-sys-color-surface-container);
    color: var(--md-sys-color-on-surface);
    font-size: 12px;
    cursor: pointer;
  }

  .candidate-chip:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  .env-editor-row {
    display: flex;
    gap: 8px;
  }

  .env-table {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 8px;
  }

  .env-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    background: var(--md-sys-color-surface-container-high);
    border-radius: var(--md-sys-shape-small);
    font-size: 12px;
    color: var(--md-sys-color-on-surface);
  }

  .env-remove-btn {
    margin-left: auto;
    background: transparent;
    border: none;
    color: var(--md-sys-color-error);
    cursor: pointer;
  }

  /* Diagnostics */
  .diag-header-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }

  .diag-checks-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .diag-check {
    padding: 10px 14px;
    border-radius: var(--md-sys-shape-small);
    font-size: 13px;
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .check-success {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
  }

  .check-info {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .check-warning {
    background: var(--md-sys-color-tertiary-container);
    color: var(--md-sys-color-on-tertiary-container);
  }

  .check-error {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  .diag-severity-tag {
    font-size: 10px;
    font-weight: 700;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.25);
  }

  /* Sidebar */
  .steam-capsule-card {
    width: 100%;
    aspect-ratio: 16 / 9;
    border-radius: var(--md-sys-shape-large);
    overflow: hidden;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
  }

  .capsule-img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .capsule-placeholder {
    width: 100%;
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 12px;
    box-sizing: border-box;
    font-size: 13px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface-variant);
    text-align: center;
    background: linear-gradient(
      135deg,
      var(--md-sys-color-surface-container-high) 0%,
      var(--md-sys-color-surface-container-highest) 100%
    );
  }

  .sidebar-details-card {
    background: var(--md-sys-color-surface-container);
    border-radius: var(--md-sys-shape-large);
    border: 1px solid var(--md-sys-color-outline-variant);
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .sidebar-title {
    font-size: 15px;
    font-weight: 700;
    margin: 0;
    color: var(--md-sys-color-on-surface);
  }

  .sidebar-data-row {
    display: flex;
    justify-content: space-between;
    font-size: 13px;
    gap: 8px;
    align-items: center;
  }

  .data-label {
    color: var(--md-sys-color-on-surface-variant);
  }

  .data-value {
    color: var(--md-sys-color-on-surface);
    font-weight: 500;
    text-align: right;
  }

  .path-with-open {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 180px;
  }

  .path-val {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }

  .mini-icon-btn {
    width: 24px;
    height: 24px;
    border-radius: var(--md-sys-shape-full);
    border: none;
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface-variant);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    flex-shrink: 0;
    transition: background 0.15s ease, color 0.15s ease;
  }

  .mini-icon-btn:hover {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  /* MD3 More Options Menu */
  .menu-container {
    position: relative;
  }

  .menu-scrim {
    position: fixed;
    inset: 0;
    z-index: 99;
    background: transparent;
    border: none;
    padding: 0;
    margin: 0;
    cursor: default;
  }

  .md3-dropdown-menu {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 100;
    min-width: 220px;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-medium);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    padding: 6px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .menu-item {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 10px 14px;
    border-radius: var(--md-sys-shape-small);
    border: none;
    background: transparent;
    color: var(--md-sys-color-on-surface);
    font-size: 0.85rem;
    font-weight: 500;
    cursor: pointer;
    text-align: left;
    transition: background 0.15s ease;
  }

  .menu-item svg {
    fill: currentColor;
    flex-shrink: 0;
  }

  .menu-item:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  .menu-item.danger {
    color: var(--md-sys-color-error);
  }

  .menu-item.danger:hover {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  .menu-divider {
    height: 1px;
    background: var(--md-sys-color-outline-variant);
    margin: 4px 6px;
  }

  .icon-btn {
    width: 40px;
    height: 40px;
    border-radius: var(--md-sys-shape-full);
    border: none;
    background: transparent;
    color: var(--md-sys-color-on-surface-variant);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background 0.15s ease, color 0.15s ease;
  }

  .icon-btn svg {
    fill: currentColor;
  }

  .icon-btn.tonal {
    background: var(--md-sys-color-surface-container);
    color: var(--md-sys-color-on-surface);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .icon-btn.tonal:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  /* Move Modal Body */
  .move-modal-body {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }

  .modal-description {
    font-size: 14px;
    color: var(--md-sys-color-on-surface-variant);
    margin: 0;
  }

  .modal-actions-row {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 10px;
  }

  @media (max-width: 900px) {
    .steam-content-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
