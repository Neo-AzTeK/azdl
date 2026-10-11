<script>
  import { FAB } from 'm3-svelte';
  import iconPlay from '@ktibow/iconset-material-symbols/play-arrow';
  import iconStop from '@ktibow/iconset-material-symbols/stop';
  import iconDownload from '@ktibow/iconset-material-symbols/download';
  import { launchGame, stopGame } from '../api.js';
  import { getGameCardMetadataTags, normalizeGameCard } from '../game_card.js';
  import { getSteamStorageSize } from '../steam_storage.js';
  import {
    extractSteamAppId,
    getCandidateThumbnails,
    getActiveThumbnailPair,
    getActiveDisplayThumbnail,
    getResolvedArtwork,
    fetchArtwork,
    normalizeQuery,
    subscribeArtwork,
  } from '../covers.js';

  let {
    game,
    activeJob = null,
    onSelect = null,
    onLaunch = null,
    onStop = null,
    onInstall = null,
  } = $props();

  let isStartingInstall = $state(false);
  let launching = $state(false);
  let currentGameId = $state(null);
  let failedUrls = $state(new Set());
  let loadedHqUrl = $state(null);
  let loadedFallbackUrl = $state(null);
  let resolvedArt = $state(null);
  let artworkLookupKey = $state('');
  let steamStorageSize = $state(null);
  let cardGame = $derived(normalizeGameCard(game));

  $effect(() => {
    const id = game?.id || game?.qualified_id;
    if (id !== currentGameId) {
      currentGameId = id;
      failedUrls = new Set();
      loadedHqUrl = null;
      loadedFallbackUrl = null;
    }
  });

  $effect(() => {
    const titleOrId = game?.title || game?.id;
    if (titleOrId) {
      resolvedArt = getResolvedArtwork(titleOrId);
      artworkLookupKey = '';
    }
  });

  $effect(() => {
    const titleOrId = game?.title || game?.id;
    if (!titleOrId || activeHqUrl || artworkLookupKey === normalizeQuery(titleOrId)) return;
    const key = normalizeQuery(titleOrId);
    artworkLookupKey = key;
    fetchArtwork(titleOrId).then((art) => {
      if (art && normalizeQuery(game?.title || game?.id) === key) resolvedArt = art;
    });
  });

  $effect(() => {
    const key = normalizeQuery(game?.title || game?.id);
    return subscribeArtwork((_, entry) => (resolvedArt = entry), key);
  });

  let isLibraryCard = $derived(Boolean(cardGame.install_dir || cardGame.executable));
  let isInstalled = $derived(Boolean(isLibraryCard || cardGame.is_installed));
  let metadataTags = $derived(getGameCardMetadataTags(cardGame, steamStorageSize));

  function loadStorageWhenVisible(node) {
    const appId = extractSteamAppId(cardGame);
    if (isLibraryCard || cardGame.file_size || !appId) return;

    let observer;
    let destroyed = false;
    const load = () => {
      observer?.disconnect();
      getSteamStorageSize(appId).then((size) => {
        if (!destroyed && size && extractSteamAppId(cardGame) === appId) {
          steamStorageSize = size;
        }
      });
    };

    if (typeof IntersectionObserver === 'undefined') {
      load();
    } else {
      observer = new IntersectionObserver(([entry]) => {
        if (entry?.isIntersecting) load();
      }, { rootMargin: '200px' });
      observer.observe(node);
    }
    return { destroy() { destroyed = true; observer?.disconnect(); } };
  }

  let isShimmering = $derived(
    Boolean(game.active_operation) ||
    launching ||
    Boolean(game.is_running) ||
    Boolean(activeJob) ||
    isStartingInstall
  );

  let operationLabel = $derived(
    game.active_operation
      ? game.active_operation.charAt(0).toUpperCase() + game.active_operation.slice(1)
      : launching
        ? 'Launching...'
        : null
  );



  // Candidate thumbnail URLs in priority order
  let candidateUrls = $derived(getCandidateThumbnails(cardGame, resolvedArt));

  // Active pair for concurrent HQ and fallback loading
  let thumbnailPair = $derived(getActiveThumbnailPair(cardGame, resolvedArt, failedUrls));
  let activeHqUrl = $derived(thumbnailPair.activeHqUrl);
  let activeFallbackUrl = $derived(thumbnailPair.activeFallbackUrl);

  let isHqLoaded = $derived(Boolean(activeHqUrl && loadedHqUrl === activeHqUrl));
  let isFallbackLoaded = $derived(Boolean(activeFallbackUrl && loadedFallbackUrl === activeFallbackUrl));

  let activeDisplay = $derived(getActiveDisplayThumbnail({
    activeHqUrl,
    activeFallbackUrl,
    isHqLoaded,
    isFallbackLoaded,
  }));

  // Current active thumbnail URL (HQ if loaded, else fallback if loaded, else null)
  let activeThumbnail = $derived(activeDisplay.url);

  function handleHqLoaded(url) {
    if (url && url === activeHqUrl) {
      loadedHqUrl = url;
    }
  }

  function handleFallbackLoaded(url) {
    if (url && url === activeFallbackUrl) {
      loadedFallbackUrl = url;
    }
  }

  function handleHqError(url) {
    if (url) {
      failedUrls.add(url);
      failedUrls = new Set(failedUrls);
      if (loadedHqUrl === url) {
        loadedHqUrl = null;
      }
    }
  }

  function handleFallbackError(url) {
    if (url) {
      failedUrls.add(url);
      failedUrls = new Set(failedUrls);
      if (loadedFallbackUrl === url) {
        loadedFallbackUrl = null;
      }
    }
  }

  function imageLoader(node, callbacks) {
    function check() {
      if (node.complete && (node.currentSrc || node.src)) {
        if (node.naturalWidth > 0) {
          callbacks?.onLoad?.();
        } else if (node.naturalWidth === 0) {
          callbacks?.onError?.();
        }
      }
    }
    queueMicrotask(check);
    return {
      update(newCallbacks) {
        callbacks = newCallbacks;
        queueMicrotask(check);
      },
    };
  }



  async function handlePlay(e) {
    e.stopPropagation();
    if (game.is_running) {
      if (onStop) {
        onStop(game.id);
      } else {
        await stopGame(game.id);
      }
      return;
    }

    launching = true;
    try {
      if (onLaunch) {
        await onLaunch(game.id);
      } else {
        await launchGame(game.id);
      }
    } catch (err) {
      console.error('Launch failed:', err);
    } finally {
      launching = false;
    }
  }

  async function handleCatalogAction(e) {
    e.stopPropagation();
    if (onInstall) {
      isStartingInstall = true;
      try {
        await onInstall(game);
      } finally {
        isStartingInstall = false;
      }
    } else if (onSelect) {
      onSelect(game);
    }
  }
</script>

<div
  class="game-card"
  use:loadStorageWhenVisible
  class:shimmer-active={isShimmering}
  onclick={() => onSelect && onSelect(game)}
  role="button"
  tabindex="0"
  onkeydown={(e) => e.key === 'Enter' && onSelect && onSelect(game)}
>
  <!-- Card Artwork Cover Container (16:9 horizontal landscape aspect ratio) -->
  <div class="cover-container">
    {#if activeFallbackUrl}
      <img
        src={activeFallbackUrl}
        alt={cardGame.title}
        class="cover-img banner-fit fallback-layer"
        class:is-visible={isFallbackLoaded}
        loading="lazy"
        decoding="async"
        onload={() => handleFallbackLoaded(activeFallbackUrl)}
        onerror={(event) => handleFallbackError(event.currentTarget.currentSrc || event.currentTarget.src || activeFallbackUrl)}
        use:imageLoader={{
          onLoad: () => handleFallbackLoaded(activeFallbackUrl),
          onError: () => handleFallbackError(activeFallbackUrl),
        }}
      />
    {/if}

    {#if activeHqUrl}
      <img
        src={activeHqUrl}
        alt={cardGame.title}
        class="cover-img banner-fit hq-layer"
        class:is-visible={isHqLoaded}
        loading="lazy"
        decoding="async"
        onload={() => handleHqLoaded(activeHqUrl)}
        onerror={(event) => handleHqError(event.currentTarget.currentSrc || event.currentTarget.src || activeHqUrl)}
        use:imageLoader={{
          onLoad: () => handleHqLoaded(activeHqUrl),
          onError: () => handleHqError(activeHqUrl),
        }}
      />
    {/if}

    {#if !activeDisplay.url}
      <div class="cover-placeholder">
        <div class="placeholder-content">
          <svg viewBox="0 0 24 24" width="36" height="36" fill="currentColor" class="placeholder-icon">
            <path d="M21 6H3c-1.1 0-2 .9-2 2v8c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm-10 7H8v3H6v-3H3v-2h3V8h2v3h3v2zm4.5 2c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm4-3c-.83 0-1.5-.67-1.5-1.5S18.67 9 19.5 9s1.5.67 1.5 1.5-.67 1.5-1.5 1.5z"/>
          </svg>
          <span class="placeholder-title">{cardGame.title}</span>
        </div>
      </div>
    {/if}

    <!-- Active Operation Badge (Installing / Launching) -->
    {#if operationLabel}
      <div class="operation-badge">
        <span>{operationLabel}</span>
      </div>
    {/if}

    <!-- Floating Action Button (FAB) at corner of artwork -->
    <div class="card-fab-wrap" class:card-fab-stop={game.is_running}>
      {#if game.is_running}
        <FAB
          size="small"
          color="primary"
          icon={iconStop}
          onclick={handlePlay}
          aria-label="Stop game"
        />
      {:else if isInstalled}
        <FAB
          size="small"
          color="primary"
          icon={iconPlay}
          onclick={handlePlay}
          aria-label="Play game"
        />
      {:else if activeJob || isStartingInstall}
        <!-- Downloading: FAB disappears, nothing in its place -->
      {:else}
        <FAB
          size="small"
          color="primary"
          icon={iconDownload}
          onclick={handleCatalogAction}
          aria-label="Download game"
        />
      {/if}
    </div>
  </div>

  <!-- Card Info Section -->
  <div class="card-info">
    <div class="title-row">
      <span class="title" title={cardGame.title}>{cardGame.title}</span>
    </div>

    {#if metadataTags.length}
      <div class="badge-row">
        {#each metadataTags as tag (tag.type)}
          <span class="tag-badge size-tag" title={isLibraryCard ? 'Installed files on disk' : 'Minimum PC storage listed by Steam'}>
            {tag.label}
          </span>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  /* Material Design 3 Card (No motion/bouncy transforms) */
  .game-card {
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-large, 16px);
    overflow: hidden;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    position: relative;
    user-select: none;
    -webkit-user-select: none;
    box-shadow: var(--md-sys-elevation-1);
    transition: background-color 0.15s ease, border-color 0.15s ease, box-shadow 0.15s ease;
  }

  .game-card:hover {
    background: var(--md-sys-color-surface-container-high);
    border-color: var(--md-sys-color-outline);
    box-shadow: var(--md-sys-elevation-2);
  }


  /* 16:9 Horizontal Landscape Cover */
  .cover-container {
    position: relative;
    width: 100%;
    aspect-ratio: 16 / 9;
    background: var(--md-sys-color-surface-container-high);
    overflow: hidden;
  }

  .cover-img {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    opacity: 0;
    transition: opacity 0.2s ease-in;
    pointer-events: none;
  }

  .cover-img.banner-fit {
    object-position: center;
  }

  .fallback-layer {
    z-index: 1;
  }

  .hq-layer {
    z-index: 2;
  }

  .cover-img.is-visible {
    opacity: 1;
  }

  .cover-placeholder {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    z-index: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: linear-gradient(
      135deg,
      var(--md-sys-color-surface-container-high) 0%,
      var(--md-sys-color-surface-container-highest) 100%
    );
    color: var(--md-sys-color-on-surface-variant);
    padding: 12px;
    box-sizing: border-box;
  }

  .placeholder-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    text-align: center;
    max-width: 90%;
  }

  .placeholder-icon {
    color: var(--md-sys-color-primary);
    opacity: 0.6;
  }

  .placeholder-title {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--md-sys-color-on-surface-variant);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }

  .operation-badge {
    position: absolute;
    top: 8px;
    left: 8px;
    background: rgba(0, 0, 0, 0.75);
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
    color: white;
    font-size: 11px;
    font-weight: 600;
    padding: 3px 8px;
    border-radius: var(--md-sys-shape-full, 9999px);
    display: flex;
    align-items: center;
    gap: 6px;
    z-index: 10;
  }


  /* FAB Container at bottom-right of cover */
  .card-fab-wrap {
    position: absolute;
    bottom: 8px;
    right: 8px;
    z-index: 10;
    display: flex;
    align-items: center;
  }

  .card-fab-wrap :global(button) {
    width: 40px !important;
    height: 40px !important;
    padding: 0 !important;
    border-radius: var(--md-sys-shape-medium, 12px) !important;
    box-shadow: var(--md-sys-elevation-3) !important;
  }

  .card-fab-wrap.card-fab-stop :global(button) {
    background-color: var(--md-sys-color-error) !important;
    color: var(--md-sys-color-on-error) !important;
  }



  /* Card Info Area */
  .card-info {
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .title {
    font-size: 13px;
    font-weight: 700;
    color: var(--md-sys-color-on-surface);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    width: 100%;
  }

  /* Badges Row (Material 3 Tag Chips) */
  .badge-row {
    display: flex;
    align-items: center;
    gap: 5px;
    flex-wrap: wrap;
  }

  .tag-badge {
    font-size: 10px;
    font-weight: 600;
    padding: 2px 7px;
    border-radius: var(--md-sys-shape-small, 6px);
    border: 1px solid transparent;
  }

  .size-tag {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface-variant);
    border-color: var(--md-sys-color-outline-variant);
  }

</style>
