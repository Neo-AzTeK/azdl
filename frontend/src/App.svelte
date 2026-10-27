<script>
  import { onMount } from 'svelte';
  import Titlebar from './components/Titlebar.svelte';
  import NavRail from './components/NavRail.svelte';
  import TransferDrawer from './components/TransferDrawer.svelte';
  import LibraryView from './views/LibraryView.svelte';
  import ConfirmModal from './components/ConfirmModal.svelte';

  import {
    getState,
    getProgress,
    getRunners,
    launchGame,
    stopGame,
    onBusyChange,
    onStateChange,
    onProgressChange,
    isSteamRunning,
  } from './api.js';
  import { applyTheme, onSystemThemeChange } from './theme.js';
  import { notifications, dismissNotification, notify, notifyError } from './notifications.js';
  import { reconcileData, createRefreshCoordinator, progressTopology, applyProgressSnapshot } from './state.js';
  import { detailGameIdentity } from './detail_tabs.js';

  const viewModules = new Map();
  const viewLoaders = {
    catalog: () => import('./views/CatalogView.svelte'),
    settings: () => import('./views/SettingsView.svelte'),
    game: () => import('./views/GamePageView.svelte'),
  };
  function loadView(name) {
    if (!viewModules.has(name)) {
      viewModules.set(name, viewLoaders[name]().catch((error) => {
        viewModules.delete(name);
        notify.error(`Could not open this view: ${error.message}`);
        throw error;
      }));
    }
    return viewModules.get(name);
  }

  // Navigation state
  let activeTab = $state('library'); // 'library' | 'catalog' | 'settings' | 'game'
  let previousTab = $state('library');

  // Per-tab independent temporary search queries
  let tabSearches = $state({
    library: '',
    catalog: '',
    settings: '',
    game: '',
  });

  // Current search query displayed in the Titlebar
  let searchQuery = $state('');
  let activeNavTab = 'library';

  // React to tab switching by saving/restoring each tab's temporary query
  $effect(() => {
    const tab = activeTab;
    if (tab !== activeNavTab) {
      activeNavTab = tab;
      searchQuery = tabSearches[tab] || '';
    }
  });

  // Keep tabSearches synchronized with Titlebar input
  $effect(() => {
    tabSearches[activeTab] = searchQuery;
  });

  // Viewed game for dedicated Game Page view
  let viewedGame = $state(null);

  // App core state from Rust backend
  let appState = $state({
    library: [],
    plugins: [],
    plugin_store: [],
    jobs: [],
    running_games: {},
    tasks: [],
    pending: [],
    data_dir: '',
    platform: 'linux',
  });

  let runners = $state([]);
  let isApiBusy = $state(false);
  let dlcRevision = $state(0);

  // Derive active downloads, unpacks, or jobs
  let hasActiveJobs = $derived(
    (appState.jobs || []).some((j) => {
      const s = (j.status || '').toLowerCase();
      const instS = (j.install_state || '').toLowerCase();
      return (
        ['running', 'queued', 'downloading', 'working', 'pending', 'starting', 'extracting'].includes(s) ||
        ['pending', 'installing'].includes(instS)
      );
    })
  );

  // Derive active background tasks
  let hasActiveTasks = $derived(
    (appState.tasks || []).some((t) =>
      ['running', 'working', 'active'].includes((t.status || '').toLowerCase())
    )
  );

  // Discrete global "doing something" indicator state (downloads use their own progress bar in transfers)
  let isAppDoingSomething = $derived(isApiBusy);

  let settingsCategory = $state('storage');

  function handleOpenPlugins() {
    settingsCategory = 'plugins';
    activeTab = 'settings';
  }

  function safeStorageGet(key, fallback) {
    try {
      if (typeof window !== 'undefined' && window.localStorage) {
        return localStorage.getItem(key) || fallback;
      }
    } catch {}
    return fallback;
  }

  function safeStorageSet(key, val) {
    try {
      if (typeof window !== 'undefined' && window.localStorage) {
        localStorage.setItem(key, val);
      }
    } catch {}
  }

  // Theming & Personalization state
  let seedColor = $state(safeStorageGet('azdl_seed_color', '#10B981'));

  let themeMode = $state(safeStorageGet('azdl_theme_mode', 'dark'));

  function showToast(message, type = 'info') {
    notify(message, type);
  }

  let isMounted = false;
  let pollTimeoutId = null;
  let lastFullRefreshAt = 0;
  let lastProgressAt = 0;
  let fullRefreshGeneration = 0;
  let progressRevision = 0;

  function applyProgress(progress) {
    if (!isMounted || !progress) return;
    lastProgressAt = Date.now();
    const next = applyProgressSnapshot(appState, progress);
    if (next === appState) return;
    const changed = progressTopology(appState) !== progressTopology(progress);
    progressRevision++;
    appState = next;
    if (changed) void refreshState(false);
  }

  const { refreshFull: refreshState, refreshProgress } = createRefreshCoordinator(fetchFullState, async () => {
    const generation = fullRefreshGeneration;
    try {
      const progress = await getProgress();
      if (generation === fullRefreshGeneration) applyProgress(progress);
    } catch (error) {
      console.warn('Could not refresh transfer progress:', error);
    }
  });

  function scheduleAdaptivePoll() {
    if (!isMounted) return;
    if (pollTimeoutId) {
      clearTimeout(pollTimeoutId);
      pollTimeoutId = null;
    }
    // Fast polling (1s) when downloads, unpacks or tasks are active;
    // relaxed polling (15s) when the launcher is idle.
    const delay = (hasActiveJobs || hasActiveTasks) ? 1000 : 15000;
    pollTimeoutId = setTimeout(async () => {
      if (!isMounted) return;
      if (document.visibilityState === 'visible') {
        if (Date.now() - lastFullRefreshAt >= 30000) await refreshState(false);
        else if ((hasActiveJobs || hasActiveTasks) && Date.now() - lastProgressAt >= 3000) await refreshProgress();
      }
      scheduleAdaptivePoll();
    }, delay);
  }

  // React to transitions between active and idle states to adjust polling cadence immediately
  $effect(() => {
    const _isActive = hasActiveJobs || hasActiveTasks;
    if (isMounted) {
      scheduleAdaptivePoll();
    }
  });

  onMount(() => {
    isMounted = true;

    // Initial theme application
    updateTheme();

    // Listen for OS system theme changes
    const unsubSystemTheme = onSystemThemeChange(() => {
      if (themeMode === 'system') {
        updateTheme();
      }
    });

    // Listen for active API requests
    const unsubBusy = onBusyChange((busy) => {
      isApiBusy = busy;
    });

    // Listen for backend state change events (Tauri IPC event)
    const unsubStateChange = onStateChange(() => {
      if (document.visibilityState === 'visible') {
        refreshState(false);
      }
    });
    const unsubProgressChange = onProgressChange((event) => {
      if (document.visibilityState === 'visible') applyProgress(event.payload);
    });

    // Fetch initial backend state
    refreshState();
    refreshRunners();

    const refreshWhenVisible = () => {
      if (document.visibilityState === 'visible') {
        refreshState(false);
        scheduleAdaptivePoll();
      }
    };
    document.addEventListener('visibilitychange', refreshWhenVisible);

    // Initial adaptive polling schedule
    scheduleAdaptivePoll();

    return () => {
      isMounted = false;
      unsubSystemTheme();
      unsubBusy();
      unsubStateChange();
      unsubProgressChange();
      document.removeEventListener('visibilitychange', refreshWhenVisible);
      if (pollTimeoutId) {
        clearTimeout(pollTimeoutId);
        pollTimeoutId = null;
      }
    };
  });

  function updateTheme() {
    safeStorageSet('azdl_seed_color', seedColor);
    safeStorageSet('azdl_theme_mode', themeMode);
    applyTheme(seedColor, themeMode);
  }

  const seenTaskFailures = new Map();
  async function fetchFullState(showErrors = true) {
    try {
        fullRefreshGeneration++;
        const revision = progressRevision;
        let state = await getState();
        if (state) {
          lastFullRefreshAt = Date.now();
          if (revision !== progressRevision) state = { ...state, jobs: appState.jobs, tasks: appState.tasks, running_games: appState.running_games };
          const failedIds = new Set();
          for (const task of state.tasks || []) {
            if (task.status !== 'failed') continue;
            failedIds.add(task.id);
            if (seenTaskFailures.get(task.id) !== task.log_tail) {
              seenTaskFailures.set(task.id, task.log_tail);
              notify.error(task.log_tail || `${task.label} failed.`);
            }
          }
          for (const id of seenTaskFailures.keys()) {
            if (!failedIds.has(id)) seenTaskFailures.delete(id);
          }
          const reconciledState = reconcileData(appState, state);
          if (reconciledState !== appState) appState = reconciledState;
          // Keep viewed game reference in sync
          if (viewedGame) {
            const vId = (viewedGame.id || '').toLowerCase();
            const vQId = (viewedGame.qualified_id || '').toLowerCase();
            const vTitle = (viewedGame.title || '').trim().toLowerCase();
            const updated = (state.library || []).find(
              (g) =>
                (g.id && g.id.toLowerCase() === vId) ||
                (g.qualified_id && g.qualified_id.toLowerCase() === vQId) ||
                (g.title && g.title.trim().toLowerCase() === vTitle)
            );
            if (updated) {
              const reconciledGame = reconcileData(viewedGame, { ...viewedGame, ...updated });
              if (reconciledGame !== viewedGame) viewedGame = reconciledGame;
            }
          }
        }
    } catch (e) {
      if (showErrors) console.error('Failed to get state:', e);
    }
  }

  async function refreshRunners() {
    try {
      runners = await getRunners();
    } catch (e) {
      console.error('Failed to get runners:', e);
    }
  }

  function handleSelectGame(game, sourceTab = activeTab) {
    if (sourceTab !== 'game') {
      previousTab = sourceTab;
    }
    viewedGame = game;
    activeTab = 'game';
  }

  function handleBackFromGame() {
    activeTab = previousTab || 'library';
    viewedGame = null;
  }

  let showSteamPromptModal = $state(false);
  let pendingSteamLaunchGame = $state(null);

  async function executeLaunch(gameId) {
    try {
      await launchGame(gameId);
      await refreshState();
    } catch (e) {
      notifyError(e, {
        onOpenSettings: (category) => {
          activeTab = 'settings';
          settingsCategory = category || 'runners';
        },
      });
    }
  }

  async function handleLaunch(gameId) {
    const game = (appState.library || []).find((g) => g.id === gameId);
    const needsOnlineFix = game && (game.fix === 'Online-Fix' || game.fix === 'SteamFix');
    if (needsOnlineFix) {
      try {
        const running = await isSteamRunning();
        if (!running) {
          pendingSteamLaunchGame = game;
          showSteamPromptModal = true;
          return;
        }
      } catch (err) {
        console.warn('Steam running check failed:', err);
      }
    }
    await executeLaunch(gameId);
  }

  async function handleLaunchWithoutSteam() {
    if (!pendingSteamLaunchGame) return;
    const targetGame = pendingSteamLaunchGame;
    pendingSteamLaunchGame = null;
    await executeLaunch(targetGame.id);
  }

  async function handleStop(gameId) {
    try {
      await stopGame(gameId);
      await refreshState();
    } catch (e) {
      notifyError(e);
    }
  }
</script>

<div class="app-layout">
  <!-- Frameless MD3 Titlebar with Drag Area, Global Search, Active Game Pill -->
  <Titlebar
    bind:searchQuery
    {activeTab}
    runningGames={appState.running_games}
    onStopGame={handleStop}
  />

  <!-- Discrete Material 3 App-wide "Doing Something" Indeterminate Loader Bar -->
  <div
    class="app-global-loader"
    class:active={isAppDoingSomething}
    role="progressbar"
    aria-label="Application activity indicator"
    aria-hidden={!isAppDoingSomething}
  >
    <div class="loader-track"></div>
  </div>

  <!-- Main Workspace: NavRail + Views Area -->
  <div class="main-workspace">
    <NavRail bind:activeTab />

    <main class="content-viewport">
      {#if activeTab === 'library'}
        <LibraryView
          library={appState.library}
          pending={appState.pending}
          searchQuery={tabSearches.library}
          onSelectGame={(game) => handleSelectGame(game, 'library')}
          onLaunchGame={handleLaunch}
          onStopGame={handleStop}
          onRefresh={refreshState}
          onBrowseCatalog={() => (activeTab = 'catalog')}
        />
      {:else if activeTab === 'catalog'}
        {#await loadView('catalog')}
          <div class="view-loading" role="status" aria-live="polite">
            <span class="view-loading-spinner" aria-hidden="true"></span>
            <span>Opening Discover…</span>
          </div>
        {:then module}
        {@const CatalogView = module.default}
        <CatalogView
          jobs={appState.jobs}
          tasks={appState.tasks}
          {dlcRevision}
          plugins={appState.plugins}
          settings={appState.settings}
          searchQuery={tabSearches.catalog}
          onSelectGame={(game) => handleSelectGame(game, 'catalog')}
          onRefresh={refreshState}
          onGoToSettings={() => (activeTab = 'settings')}
        />
        {:catch}
          <button class="md3-btn text" onclick={() => activeTab = 'library'}>Back to Library</button>
        {/await}
      {:else if activeTab === 'settings'}
        {#await loadView('settings')}
          <div class="view-loading" role="status" aria-live="polite">
            <span class="view-loading-spinner" aria-hidden="true"></span>
            <span>Opening Settings…</span>
          </div>
        {:then module}
        {@const SettingsView = module.default}
        <SettingsView
          bind:seedColor
          bind:themeMode
          bind:activeCategory={settingsCategory}
          {runners}
          dataDir={appState.data_dir}
          platform={appState.platform}
          plugins={appState.plugins}
          pluginStore={appState.plugin_store}
          settings={appState.settings}
          diagnostics={appState.diagnostics}
          library={appState.library}
          searchQuery={tabSearches.settings}
          onThemeChange={updateTheme}
          onRefresh={refreshState}
          onShowToast={showToast}
        />
        {:catch}
          <button class="md3-btn text" onclick={() => activeTab = 'library'}>Back to Library</button>
        {/await}
      {:else if activeTab === 'game' && viewedGame}
        {#await loadView('game')}
          <div class="view-loading" role="status" aria-live="polite">
            <span class="view-loading-spinner" aria-hidden="true"></span>
            <span>Opening game…</span>
          </div>
        {:then module}
        {@const GamePageView = module.default}
        {#key detailGameIdentity(viewedGame)}
        <GamePageView
          game={viewedGame}
          library={appState.library}
          plugins={appState.plugins}
          settings={appState.settings}
          {runners}
          jobs={appState.jobs}
          tasks={appState.tasks}
          fromTab={previousTab}
          onBack={handleBackFromGame}
          onLaunchGame={handleLaunch}
          onStopGame={handleStop}
          onOpenPlugins={handleOpenPlugins}
          onRefresh={refreshState}
          onShowToast={showToast}
        />
        {/key}
        {:catch}
          <button class="md3-btn text" onclick={handleBackFromGame}>Back</button>
        {/await}
      {/if}
    </main>
  </div>

  <!-- Persistent Bottom-Bar Transfer Drawer -->
  <TransferDrawer
    jobs={appState.jobs}
    onRefresh={refreshState}
    onDlcChanged={() => { dlcRevision += 1; }}
  />

  <!-- Toast Snackbars Stack -->
  {#if $notifications.length > 0}
    <div class="toast-stack-container" role="status" aria-live="polite">
      {#each $notifications as n (n.id)}
        <div class="toast-snackbar {n.type}">
          <span class="toast-text">{n.message}</span>
          {#if n.action}
            <button
              type="button"
              onclick={() => {
                try {
                  n.action.onClick();
                } catch (err) {
                  console.error('Error executing toast action:', err);
                } finally {
                  dismissNotification(n.id);
                }
              }}
              class="toast-action-btn"
            >
              {n.action.label}
            </button>
          {/if}
          <button onclick={() => dismissNotification(n.id)} class="toast-dismiss" aria-label="Dismiss">✕</button>
        </div>
      {/each}
    </div>
  {/if}

  <!-- Online-Fix compatibility notice -->
  <ConfirmModal
    bind:open={showSteamPromptModal}
    title="Steam Is Not Running"
    message="{pendingSteamLaunchGame?.title || 'This game'} uses an Online-Fix / SteamFix multiplayer runtime that may require Steam running in the background."
    confirmText="Launch Anyway"
    cancelText="Cancel"
    isDestructive={false}
    onConfirm={handleLaunchWithoutSteam}
    onCancel={() => { pendingSteamLaunchGame = null; }}
  />
</div>

<style>
  .app-layout {
    width: 100vw;
    height: 100vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  /* Discrete App-wide "Doing Something" Indeterminate Linear Loader Bar */
  .app-global-loader {
    position: relative;
    width: 100%;
    height: 2.5px;
    margin-bottom: -2.5px;
    z-index: 999;
    pointer-events: none;
    overflow: hidden;
    opacity: 0;
    background: transparent;
    transition: opacity 0.25s cubic-bezier(0.4, 0, 0.2, 1);
  }

  .app-global-loader.active {
    opacity: 1;
    background: rgba(var(--md-sys-color-primary-rgb, 16, 185, 129), 0.16);
  }

  .app-global-loader .loader-track {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    overflow: hidden;
  }

  .app-global-loader.active .loader-track::before,
  .app-global-loader.active .loader-track::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--md-sys-color-primary, #10b981);
    box-shadow: 0 0 8px var(--md-sys-color-primary, #10b981);
    border-radius: 2px;
    will-change: left, right;
  }

  .app-global-loader.active .loader-track::before {
    animation: md3-indeterminate-1 2.1s cubic-bezier(0.65, 0.815, 0.735, 0.395) infinite;
  }

  .app-global-loader.active .loader-track::after {
    animation: md3-indeterminate-2 2.1s cubic-bezier(0.165, 0.84, 0.44, 1) 1.15s infinite;
  }

  @keyframes md3-indeterminate-1 {
    0% {
      left: -35%;
      right: 100%;
    }
    60% {
      left: 100%;
      right: -90%;
    }
    100% {
      left: 100%;
      right: -90%;
    }
  }

  @keyframes md3-indeterminate-2 {
    0% {
      left: -200%;
      right: 100%;
    }
    60% {
      left: 107%;
      right: -8%;
    }
    100% {
      left: 107%;
      right: -8%;
    }
  }

  .main-workspace {
    flex: 1;
    display: flex;
    overflow: hidden;
    position: relative;
  }

  .content-viewport {
    flex: 1;
    height: 100%;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    background: var(--md-sys-color-surface);
  }

  .view-loading {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
    color: var(--md-sys-color-on-surface-variant);
    font-size: 14px;
    font-weight: 500;
    text-align: center;
  }

  .view-loading-spinner {
    width: 28px;
    height: 28px;
    border: 3px solid var(--md-sys-color-outline-variant);
    border-top-color: var(--md-sys-color-primary);
    border-radius: var(--md-sys-shape-full);
    animation: view-loading-spin 800ms linear infinite;
  }

  @keyframes view-loading-spin {
    to { transform: rotate(360deg); }
  }

  @media (prefers-reduced-motion: reduce) {
    .view-loading-spinner {
      animation-duration: 1800ms;
    }
  }

  .toast-stack-container {
    position: fixed;
    bottom: 24px;
    right: 24px;
    display: flex;
    flex-direction: column-reverse;
    gap: 8px;
    max-width: 420px;
    z-index: 9999;
    pointer-events: none;
  }

  .toast-snackbar {
    pointer-events: auto;
    padding: 12px 18px;
    border-radius: var(--md-sys-shape-medium, 14px);
    font-size: 0.85rem;
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 12px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
    animation: toast-in 0.2s ease-out;
  }

  .toast-snackbar.info {
    background: var(--md-sys-color-surface-container-highest, #282828);
    color: var(--md-sys-color-on-surface, #e2e2e2);
    border: 1px solid var(--md-sys-color-outline-variant, rgba(255, 255, 255, 0.15));
  }

  .toast-snackbar.error {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
    border: 1px solid var(--md-sys-color-error, rgba(255, 0, 0, 0.3));
  }

  .toast-snackbar.success {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .toast-snackbar.warning {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
    border: 1px solid var(--md-sys-color-outline-variant);
  }

  .toast-action-btn {
    background: var(--md-sys-color-primary, #6750a4);
    color: var(--md-sys-color-on-primary, #ffffff);
    border: none;
    border-radius: var(--md-sys-shape-full, 9999px);
    padding: 4px 12px;
    font-size: 0.8125rem;
    font-weight: 600;
    cursor: pointer;
    white-space: nowrap;
    transition: filter 0.15s ease, opacity 0.15s ease;
    flex-shrink: 0;
  }

  .toast-action-btn:hover {
    filter: brightness(1.15);
  }

  .toast-snackbar.error .toast-action-btn {
    background: var(--md-sys-color-error, #b3261e);
    color: var(--md-sys-color-on-error, #ffffff);
  }

  .toast-dismiss {
    background: transparent;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: 0.9rem;
    padding: 2px 6px;
    opacity: 0.7;
    margin-left: auto;
  }

  .toast-dismiss:hover {
    opacity: 1;
  }

  @keyframes toast-in {
    from {
      opacity: 0;
      transform: translateY(10px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
</style>
