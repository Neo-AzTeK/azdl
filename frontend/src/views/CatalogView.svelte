<script>
  import { onMount } from 'svelte';
  import { searchSteamGames, getSteamDetails, runAction } from '../api.js';
  import { notify, notifyError } from '../notifications.js';
  import GameCard from '../components/GameCard.svelte';
  import SectionCard from '../components/SectionCard.svelte';

  const ACTIVE_JOB_STATUSES = new Set(['running', 'queued', 'downloading', 'working', 'pending']);
  const ACTIVE_INSTALL_STATES = new Set(['pending', 'installing']);
  const ACTIVE_TASK_STATUSES = new Set(['running', 'working']);

  let {
    jobs = [],
    tasks = [],
    plugins = [],
    settings = null,
    searchQuery = '',
    onSelectGame = null,
    onRefresh = null,
    onGoToSettings = null,
  } = $props();

  async function selectCatalogEntry(entry, openNormal = true) {
    if (!entry) return false;
    let parentId = Number(entry.parent_app_id);
    let parentTitle = entry.parent_title;
    let isDlc = entry.kind === 'dlc';
    if (!isDlc && entry.kind !== 'game' && entry.steam_app_id) {
      const details = await getSteamDetails(entry.steam_app_id);
      if (!details?.type) {
        notify.error('Steam could not identify this catalog entry. Try again later.');
        return true;
      }
      isDlc = String(details?.type || '').toLowerCase() === 'dlc';
      if (isDlc) {
        parentId = Number(details?.fullgame?.appid);
        parentTitle = details?.fullgame?.name;
      }
    }
    if (isDlc) {
      if (!Number.isInteger(parentId) || parentId <= 0) {
        notify.error('The parent game for this DLC is unavailable.');
        return true;
      }
      if (!parentTitle) {
        parentTitle = (await getSteamDetails(parentId))?.name;
      }
      onSelectGame?.({
        id: `steam-${parentId}`,
        qualified_id: `steam:${parentId}`,
        source_id: 'steam',
        steam_app_id: parentId,
        title: parentTitle || `Steam game ${parentId}`,
        selected_dlc_id: String(entry.steam_app_id),
      });
      return true;
    }
    if (openNormal) onSelectGame?.(entry);
    return false;
  }

  async function handleInstallGame(game) {
    if (!game) return;
    if (await selectCatalogEntry(game, false)) return;
    const favoriteId = settings?.favorite_plugin_id;
    const defaultPluginId = plugins.find((plugin) => plugin.id === favoriteId)?.id
      || (plugins.length === 1 ? plugins[0].id : null);
    if (game.source_id === 'steam' || !game.source_id) {
      if (!defaultPluginId && plugins.length > 1) {
        onSelectGame?.(game);
        return;
      }
      if (!defaultPluginId || plugins.length === 0) {
        notifyError(
          {
            code: 'no_plugin_available',
            message: 'Install a download plugin from Settings first.',
          },
          {
            onOpenSettings: () => onGoToSettings?.(),
          }
        );
        return;
      }
    }

    try {
      await runAction({
        action: 'steam-install',
        id: game.qualified_id || game.id,
        title: game.title,
        banner_url: game.banner_url || game.header_image || game.large_capsule_image || null,
        cover_url: game.cover_url || null,
        plugin_id: defaultPluginId || undefined,
      });
      if (onRefresh) onRefresh();
    } catch (err) {
      notifyError(err, {
        onOpenSettings: () => onGoToSettings?.(),
      });
    }
  }

  let loading = $state(true);
  let catalogGames = $state([]);
  let initialMount = true;

  let filteredGames = $derived(catalogGames);
  let isSearching = $derived(Boolean(searchQuery.trim()));
  let featuredGames = $derived(!isSearching ? filteredGames.slice(0, 6) : []);
  let browseGames = $derived(isSearching ? filteredGames : filteredGames.slice(6));
  let activeJobs = $derived(
    (Array.isArray(jobs) ? jobs : []).filter((job) => {
      const status = (job.status || '').toLowerCase();
      const installState = (job.install_state || '').toLowerCase();
      return ACTIVE_JOB_STATUSES.has(status) || ACTIVE_INSTALL_STATES.has(installState);
    })
  );
  let activeTasks = $derived(
    (Array.isArray(tasks) ? tasks : []).filter((task) =>
      ACTIVE_TASK_STATUSES.has((task.status || '').toLowerCase())
    )
  );

  let displayLimit = $state(60);
  let searchTimeout = null;
  let requestSerial = 0;

  function getActiveJobForGame(game) {
    if (!game) return null;
    const gTitle = (game.title || '').trim().toLowerCase();
    const gId = (game.id || '').toLowerCase();
    const qId = (game.qualified_id || '').toLowerCase();

    if (activeJobs.length > 0) {
      const match = activeJobs.find((j) => {
        // Check install metadata
        if (j.install) {
          if (
            j.install.id &&
            (j.install.id.toLowerCase() === gId || j.install.id.toLowerCase() === qId)
          ) {
            return true;
          }
          if (j.install.title && j.install.title.trim().toLowerCase() === gTitle) {
            return true;
          }
        }
        // Check target or job id
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

    if (activeTasks.length > 0) {
      const activeTask = activeTasks.find((t) => {
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
  }

  async function loadCatalog(query = searchQuery) {
    const serial = ++requestSerial;
    loading = true;
    try {
      const results = await searchSteamGames(query);
      if (serial === requestSerial) catalogGames = results;
    } catch (e) {
      if (serial === requestSerial) {
        console.error('Failed to load Steam games:', e);
        notify(`Steam discovery error: ${e}`, 'error');
      }
    } finally {
      if (serial === requestSerial) loading = false;
    }
  }

  onMount(() => {
    loadCatalog(searchQuery);
  });

  $effect(() => {
    const q = searchQuery;
    displayLimit = 60;
    if (initialMount) {
      initialMount = false;
      return;
    }
    if (searchTimeout) clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {
      loadCatalog(q);
    }, 250);

    return () => {
      if (searchTimeout) clearTimeout(searchTimeout);
    };
  });
</script>

<div class="catalog-view">
  <!-- Catalog Grid -->
  {#if loading}
    <div class="loading-state">
      <span>Finding games on Steam...</span>
    </div>
  {:else if catalogGames.length === 0}
    <div class="empty-state">
      <svg viewBox="0 0 24 24" width="48" height="48" fill="currentColor" opacity="0.4">
        <path d="M4 6H2v14c0 1.1.9 2 2 2h14v-2H4V6zm16-4H8c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm0 14H8V4h12v12z"/>
      </svg>
      <h3 class="empty-title">No Steam games found</h3>
      <p class="empty-subtext">Search by game title or Steam App ID.</p>
    </div>
  {:else if filteredGames.length === 0}
    <div class="empty-state">
      <p class="empty-subtext">No Steam games match your search.</p>
    </div>
  {:else if !isSearching}
    {#if featuredGames.length > 0}
      <SectionCard
        title="Featured on Steam"
        icon="M12 17.27 18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z"
        description="Top sellers, new releases, and current specials."
      >
        <div class="featured-grid">
          {#each featuredGames as game (game.qualified_id)}
            {@const activeJob = getActiveJobForGame(game)}
            <GameCard {game} {activeJob} onSelect={selectCatalogEntry} onInstall={handleInstallGame} />
          {/each}
        </div>
      </SectionCard>
    {/if}

    {#if browseGames.length > 0}
      <section class="browse-section" aria-label="More games to discover">
        <div class="browse-heading">
          <h2>More to discover</h2>
        </div>
        <div class="catalog-grid">
          {#each browseGames.slice(0, Math.max(0, displayLimit - featuredGames.length)) as game (game.qualified_id)}
            {@const activeJob = getActiveJobForGame(game)}
            <GameCard {game} {activeJob} onSelect={selectCatalogEntry} onInstall={handleInstallGame} />
          {/each}
        </div>
      </section>
    {/if}

    {#if filteredGames.length > displayLimit}
      <div class="load-more-container">
        <button class="load-more-btn" onclick={() => (displayLimit += 60)}>
          Load more ({filteredGames.length - displayLimit} remaining)
        </button>
      </div>
    {/if}
  {:else}
    <div class="catalog-grid">
      {#each filteredGames.slice(0, displayLimit) as game (game.qualified_id)}
        {@const activeJob = getActiveJobForGame(game)}
        <GameCard
          {game}
          {activeJob}
          onSelect={selectCatalogEntry}
          onInstall={handleInstallGame}
        />
      {/each}
    </div>

    {#if filteredGames.length > displayLimit}
      <div class="load-more-container">
        <button class="load-more-btn" onclick={() => (displayLimit += 60)}>
          Load more ({filteredGames.length - displayLimit} remaining)
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .catalog-view {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow-y: auto;
    padding: 16px 24px;
    gap: 16px;
  }

  .catalog-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 18px;
  }

  .featured-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr));
    gap: 16px;
  }

  .browse-section {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .browse-heading {
    display: flex;
    align-items: center;
    min-height: 28px;
  }

  .browse-heading h2 {
    color: var(--md-sys-color-on-surface);
    font-size: 1.05rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .load-more-container {
    display: flex;
    justify-content: center;
    padding: 24px 0 16px;
  }

  .load-more-btn {
    padding: 10px 24px;
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-primary);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-full);
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    transition: background 150ms ease;
  }

  .load-more-btn:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  .loading-state,
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 60px 0;
    text-align: center;
    font-size: 13px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .empty-title {
    margin: 12px 0 6px;
    font-size: 16px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
  }

  .empty-subtext {
    margin: 0 auto 16px;
    font-size: 13px;
    color: var(--md-sys-color-on-surface-variant);
    max-width: 420px;
    text-align: center;
    line-height: 1.5;
  }
</style>
