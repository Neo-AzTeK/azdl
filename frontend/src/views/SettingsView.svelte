<script>
  import iconArrowDropDown from '@ktibow/iconset-material-symbols/arrow-drop-down';
  import iconAdd from '@ktibow/iconset-material-symbols/add';
  import iconAddBox from '@ktibow/iconset-material-symbols/add-box-outline';
  import iconCheck from '@ktibow/iconset-material-symbols/check';
  import iconCheckCircle from '@ktibow/iconset-material-symbols/check-circle-outline';
  import iconDarkMode from '@ktibow/iconset-material-symbols/dark-mode-outline';
  import iconDelete from '@ktibow/iconset-material-symbols/delete-outline';
  import iconError from '@ktibow/iconset-material-symbols/error-outline';
  import iconFolderOpen from '@ktibow/iconset-material-symbols/folder-open-outline';
  import iconInfo from '@ktibow/iconset-material-symbols/info-outline';
  import iconInfoOutline from '@ktibow/iconset-material-symbols/info-outline';
  import iconPalette from '@ktibow/iconset-material-symbols/palette-outline';
  import iconPlayCircle from '@ktibow/iconset-material-symbols/play-circle-outline';
  import iconStar from '@ktibow/iconset-material-symbols/star-outline';
  import iconStorage from '@ktibow/iconset-material-symbols/storage';
  import iconVideogameAsset from '@ktibow/iconset-material-symbols/videogame-asset-outline';
  import { PRESET_SEEDS } from '../theme.js';
  import { displayPath } from '../paths.js';
  import { Icon } from 'm3-svelte';
  import Modal from '../components/Modal.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import SectionCard from '../components/SectionCard.svelte';
  import { notify } from '../notifications.js';
  import {
    runAction,
    updateSettings,
    addStorageDrive,
    removeStorageDrive,
    setDefaultDrive,
    moveGameToDrive,
    downloadProtonGE,
    openGameFolder,
  } from '../api.js';

  let {
    seedColor = $bindable('#10B981'),
    themeMode = $bindable('dark'), // 'system' | 'dark' | 'light' | 'amoled'
    activeCategory = $bindable('storage'), // 'storage' | 'launch' | 'appearance' | 'about'
    runners = [],
    dataDir = '',
    platform = 'linux',
    settings = null,
    diagnostics = [],
    library = [],
    searchQuery = '',
    onThemeChange,
    onRefresh,
    onRefreshRunners,
    onShowToast,
  } = $props();

  // Local working copy of settings
  let localSettings = $state({
    storage_drives: [],
    seed_color: seedColor,
    theme_mode: themeMode,
    default_runner: 'auto',
  });

  $effect(() => {
    if (settings) {
      localSettings = {
        ...localSettings,
        ...settings,
      };
    }
  });

  // Storage UI state
  let newDrivePath = $state('');
  let newDriveLabel = $state('');
  let moveGameId = $state('');
  let moveTargetDriveId = $state('');
  let movingGameId = $state(null);
  let individualTargetDrives = $state({});

  let settingsCategories = $derived([
    { id: 'storage', label: 'Storage & Drives', icon: iconStorage, keywords: ['storage', 'drive', 'disk', 'folder', 'migrate', 'path', 'games'] },
    ...(platform === 'linux'
      ? [{ id: 'launch', label: 'Compatibility Runners', icon: iconPlayCircle, keywords: ['launch', 'proton', 'wine', 'runner', 'ge', 'prefix'] }]
      : []),
    { id: 'appearance', label: 'Material You Theming', icon: iconPalette, keywords: ['theme', 'appearance', 'material', 'color', 'seed', 'dark', 'light'] },
    { id: 'about', label: 'About & Diagnostics', icon: iconInfo, keywords: ['about', 'diagnostics', 'doctor', 'version', 'system', 'bsdtar', 'curl'] },
  ]);

  function doesCategoryMatch(cat, query) {
    if (!query || !query.trim()) return false;
    const q = query.toLowerCase().trim();
    if (cat.label.toLowerCase().includes(q)) return true;
    if (cat.keywords.some(k => k.includes(q))) return true;
    return false;
  }

  $effect(() => {
    if (searchQuery !== undefined && searchQuery.trim()) {
      const q = searchQuery.toLowerCase().trim();
      const currentMatch = settingsCategories.find(c => c.id === activeCategory && doesCategoryMatch(c, q));
      if (!currentMatch) {
        const firstMatch = settingsCategories.find(c => doesCategoryMatch(c, q));
        if (firstMatch) {
          activeCategory = firstMatch.id;
        }
      }
    }
  });

  // Launch UI state
  let isDownloadingProton = $state(false);
  let savingDefaultRunner = $state(false);
  let windowsRunners = $derived(runners.filter((r) => r.runner_type !== 'native'));
  let confirmedDefaultRunner = $derived(settings?.default_runner || 'auto');

  // Appearance UI state
  let customHexInput = $state(seedColor);

  let confirmDialog = $state({
    open: false,
    title: '',
    message: '',
    confirmText: 'Confirm',
    isDestructive: true,
    action: null,
  });

  function triggerConfirm({ title, message, confirmText = 'Confirm', isDestructive = true, action }) {
    confirmDialog = {
      open: true,
      title,
      message,
      confirmText,
      isDestructive,
      action,
    };
  }

  // --- Theme Handlers ---
  function handleSelectSeed(hex) {
    seedColor = hex;
    customHexInput = hex;
    localSettings.seed_color = hex;
    saveLocalSettings();
    if (onThemeChange) onThemeChange();
  }

  function handleApplyCustomHex() {
    if (/^#[0-9A-Fa-f]{6}$/.test(customHexInput)) {
      seedColor = customHexInput;
      localSettings.seed_color = customHexInput;
      saveLocalSettings();
      if (onThemeChange) onThemeChange();
    }
  }

  function handleSelectMode(mode) {
    themeMode = mode;
    localSettings.theme_mode = mode;
    saveLocalSettings();
    if (onThemeChange) onThemeChange();
  }

  async function saveLocalSettings() {
    try {
      await updateSettings(localSettings);
      if (onRefresh) await onRefresh(false);
    } catch (e) {
      notify(`Failed to save settings: ${e}`, 'error');
    }
  }

  // --- Storage Actions ---
  async function handleAddDrive() {
    if (!newDrivePath.trim()) {
      notify('Drive path is required', 'error');
      return;
    }
    try {
      await addStorageDrive(newDriveLabel.trim(), newDrivePath.trim());
      newDrivePath = '';
      newDriveLabel = '';
      notify('Storage drive added successfully', 'info');
      if (onRefresh) await onRefresh();
    } catch (e) {
      notify(`Error adding drive: ${e}`, 'error');
    }
  }

  function requestRemoveDrive(driveId, label) {
    triggerConfirm({
      title: 'Remove Storage Drive',
      message: `Are you sure you want to remove ${label || driveId} from your storage locations?`,
      confirmText: 'Remove Drive',
      isDestructive: true,
      action: async () => {
        try {
          await removeStorageDrive(driveId);
          notify('Storage drive removed', 'info');
          if (onRefresh) await onRefresh();
        } catch (e) {
          notify(`Error removing drive: ${e}`, 'error');
        }
      },
    });
  }

  async function handleSetDefaultDrive(driveId) {
    try {
      await setDefaultDrive(driveId);
      notify('Default storage drive updated', 'info');
      if (onRefresh) await onRefresh();
    } catch (e) {
      notify(`Error setting default drive: ${e}`, 'error');
    }
  }

  async function handleMoveGame() {
    if (!moveGameId || !moveTargetDriveId) {
      notify('Please select a game and a target drive', 'error');
      return;
    }
    try {
      await moveGameToDrive(moveGameId, moveTargetDriveId);
      notify(`Game successfully moved to target drive`, 'info');
      moveGameId = '';
      if (onRefresh) await onRefresh();
    } catch (e) {
      notify(`Error moving game: ${e}`, 'error');
    }
  }

  // --- Launch Actions ---
  async function handleSetDefaultRunner(value) {
    if (savingDefaultRunner || value === confirmedDefaultRunner) return;
    savingDefaultRunner = true;
    try {
      await updateSettings({ ...localSettings, default_runner: value });
      if (onRefresh) await onRefresh(false);
      notify('Default runner updated', 'success');
    } catch (e) {
      notify(`Failed to set default runner: ${e}`, 'error');
    } finally {
      savingDefaultRunner = false;
    }
  }

  async function handleDownloadProton() {
    isDownloadingProton = true;
    notify('Downloading latest Proton-GE release...', 'info');
    try {
      const res = await downloadProtonGE();
      notify(`Proton-GE installed: ${res?.runner || 'Done'}`, 'info');
      if (onRefresh) await onRefresh();
      if (onRefreshRunners) await onRefreshRunners();
    } catch (e) {
      notify(`Failed to download Proton-GE: ${e}`, 'error');
    } finally {
      isDownloadingProton = false;
    }
  }

  async function handleMoveIndividualGame(gameId, targetDriveId) {
    if (!gameId || !targetDriveId || movingGameId) return;
    movingGameId = gameId;
    try {
      await moveGameToDrive(gameId, targetDriveId);
      notify('Game successfully moved to selected drive!', 'success');
      if (onRefresh) await onRefresh();
    } catch (e) {
      notify(`Failed to move game: ${e}`, 'error');
    } finally {
      movingGameId = null;
    }
  }

  function requestUninstallGame(gameId, title) {
    triggerConfirm({
      title: 'Uninstall Game',
      message: `Are you sure you want to completely uninstall and delete ${title || 'this game'}? All installed game files and local data will be permanently removed.`,
      confirmText: 'Uninstall',
      isDestructive: true,
      action: async () => {
        try {
          await runAction({ action: 'game-remove', id: gameId });
          notify('Game uninstalled and files removed', 'info');
          if (onRefresh) await onRefresh();
        } catch (e) {
          notify(`Failed to remove game: ${e}`, 'error');
        }
      },
    });
  }

  function formatBytes(bytes) {
    if (!bytes || bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i];
  }
</script>

<div class="settings-container">
  <!-- LEFT: MATERIAL 3 SIDENAV CATEGORY LIST -->
  <aside class="settings-sidenav">
    <div class="sidenav-header">
      <span class="sidenav-title">Settings</span>
    </div>

    <nav class="sidenav-menu">
      {#each settingsCategories as cat}
        {@const isMatch = doesCategoryMatch(cat, searchQuery)}
        <button
          class="nav-item"
          class:active={activeCategory === cat.id}
          class:search-matched={isMatch}
          onclick={() => (activeCategory = cat.id)}
        >
          <Icon icon={cat.icon} class="nav-icon" />
          <span class="nav-label">{cat.label}</span>
          {#if isMatch && searchQuery.trim()}
            <span class="match-dot" title="Matches search query"></span>
          {/if}
        </button>
      {/each}
    </nav>
  </aside>

  <!-- RIGHT: DYNAMIC CATEGORY SETTINGS VIEW -->
  <main class="settings-panel">
    <!-- 1. STORAGE & DRIVES (TWO MAIN CONTAINERS: DRIVES & GAMES) -->
    {#if activeCategory === 'storage'}
      <div class="panel-header">
        <h2>Storage & Drives Management</h2>
        <span class="subtext">Manage installation paths and relocate installed games across drives.</span>
      </div>

      <!-- CONTAINER 1: DRIVES (TO MANAGE INSTALLATION PATHS) -->
      <SectionCard
        title="Drives (Installation Paths)"
        icon={iconStorage}
        badge="{localSettings.storage_drives.length} Configured"
        description="Root storage locations where game downloads and prefixes are stored."
      >
        <div class="drives-list">
          {#each localSettings.storage_drives as drive}
            <div class="drive-card" class:default-drive={drive.is_default}>
              <div class="drive-info">
                <div class="drive-title-row">
                  <span class="drive-label">{drive.label}</span>
                  {#if drive.is_default}
                    <span class="badge default-badge">
                      <Icon icon={iconStar} size={14} />
                      Default Library
                    </span>
                  {/if}
                </div>
                <code class="drive-path" title={displayPath(drive.path)}>{displayPath(drive.path)}</code>
              </div>

              <div class="drive-actions">
                {#if !drive.is_default}
                  <button class="md3-btn tonal small" onclick={() => handleSetDefaultDrive(drive.id)}>
                    Set Default
                  </button>
                  <button class="icon-btn danger" onclick={() => requestRemoveDrive(drive.id, drive.label)} title="Remove Drive">
                    <Icon icon={iconDelete} size={18} />
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </div>

        <!-- Add New Drive Form -->
        <div class="add-drive-box">
          <h4>Add Storage Location</h4>
          <div class="form-row">
            <input
              type="text"
              class="md3-input"
              placeholder="Drive Label (e.g. Games NVMe)"
              bind:value={newDriveLabel}
            />
            <input
              type="text"
              class="md3-input flex-2"
              placeholder="Absolute Path (e.g. /mnt/games, D:\Games)"
              bind:value={newDrivePath}
            />
            <button class="md3-btn secondary" onclick={handleAddDrive}>
              <Icon icon={iconAdd} class="btn-icon" />
              Add Drive
            </button>
          </div>
        </div>
      </SectionCard>

      <!-- CONTAINER 2: GAMES (TO MANAGE GAME INSTALLATIONS & EASY MOVING) -->
      <SectionCard
        title="Games (Manage Game Installations)"
        icon={iconVideogameAsset}
        badge="{library.length} Installed"
        description="Installed games across your drives. Seamlessly relocate files, explore folders, or uninstall."
      >
        {#if library.length === 0}
          <div class="empty-state-card">
            <p class="desc">No installed games in your library yet.</p>
          </div>
        {:else}
          <div class="games-management-table">
            {#each library as game}
              <div class="game-management-row">
                <div class="game-main-info">
                  <span class="game-row-title">{game.title}</span>
                  <code class="game-row-path" title={displayPath(game.install_dir)}>{displayPath(game.install_dir)}</code>
                </div>

                <div class="game-move-controls">
                  <span class="select-field small-select">
                    <select
                      class="md3-select"
                      bind:value={individualTargetDrives[game.id]}
                    >
                      <option value="" disabled selected>Select destination drive...</option>
                      {#each localSettings.storage_drives as d}
                        <option value={d.id}>{d.label} ({displayPath(d.path)})</option>
                      {/each}
                    </select>
                    <Icon icon={iconArrowDropDown} size={20} class="select-chevron" />
                  </span>

                  <button
                    class="md3-btn tonal small"
                    onclick={() => handleMoveIndividualGame(game.id, individualTargetDrives[game.id])}
                    disabled={!individualTargetDrives[game.id] || movingGameId === game.id}
                  >
                    {movingGameId === game.id ? 'Moving...' : 'Move Game'}
                  </button>

                  <button
                    class="icon-btn tonal"
                    onclick={() => openGameFolder(game.install_dir)}
                    title="Open Installation Folder"
                  >
                    <Icon icon={iconFolderOpen} size={18} />
                  </button>

                  <button
                    class="icon-btn danger"
                    onclick={() => requestUninstallGame(game.id, game.title)}
                    title="Uninstall game"
                  >
                    <Icon icon={iconDelete} size={18} />
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </SectionCard>

    <!-- 2. LAUNCH & RUNNERS (CLEAR & UNAMBIGUOUS) -->
    {:else if activeCategory === 'launch' && platform === 'linux'}
      <div class="panel-header">
        <h2>Compatibility Runners</h2>
        <span class="subtext">Windows games run through Proton or Wine. Games set to Auto use the default below; override it per game under Launch & Runner.</span>
      </div>

      <SectionCard
        title="Default Runner"
        icon={iconVideogameAsset}
        description="Runner used by Windows games whose runner is set to Auto:"
      >
        <span class="select-field runner-select">
          <select
            class="md3-select"
            aria-label="Default runner"
            value={confirmedDefaultRunner}
            disabled={savingDefaultRunner}
            onchange={(e) => {
              const value = e.currentTarget.value;
              e.currentTarget.value = confirmedDefaultRunner;
              handleSetDefaultRunner(value);
            }}
          >
            {#each windowsRunners as r (r.id)}
              <option value={r.id}>{r.id === 'auto' ? 'Auto (Proton when Steam is installed, otherwise Wine)' : r.name}</option>
            {/each}
            {#if !windowsRunners.some((r) => r.id === confirmedDefaultRunner)}
              <option value={confirmedDefaultRunner}>{confirmedDefaultRunner} (unavailable)</option>
            {/if}
          </select>
          <Icon icon={iconArrowDropDown} size={20} class="select-chevron" />
        </span>
      </SectionCard>

      <SectionCard
        title="Proton-GE"
        icon={iconPlayCircle}
        description="Install the latest Proton-GE release so it can be selected as a game's runner:"
      >
        <div class="form-row">
          <button class="md3-btn primary" onclick={handleDownloadProton} disabled={isDownloadingProton}>
            <span>{isDownloadingProton ? 'Downloading...' : 'Download Latest Proton-GE'}</span>
          </button>
        </div>

        <div class="prefix-inline-note">
          <Icon icon={iconInfoOutline} size={16} />
          <span>
            <strong>Dedicated Prefix Isolation:</strong> each Windows game runs in its own prefix (<code>~/.local/share/azdl/prefixes/&lt;game_id&gt;</code>), keeping saves and runtime dependencies separate.
          </span>
        </div>
      </SectionCard>

    {:else if activeCategory === 'appearance'}
      <div class="panel-header">
        <h2>Material You Personalization</h2>
        <span class="subtext">Dynamic algorithmic palette generation and theme modes.</span>
      </div>

      <SectionCard
        title="Seed Color Palette"
        icon={iconPalette}
        description="Material Design 3 algorithmic tone generator extracts primary, secondary, and surface hues."
      >
        <div class="seed-swatches">
          {#each PRESET_SEEDS as seed}
            <button
              class="swatch-btn"
              class:selected={seedColor.toLowerCase() === seed.hex.toLowerCase()}
              style:background-color={seed.hex}
              onclick={() => handleSelectSeed(seed.hex)}
              title={seed.label}
            >
              {#if seedColor.toLowerCase() === seed.hex.toLowerCase()}
                <Icon icon={iconCheck} size={16} />
              {/if}
            </button>
          {/each}
        </div>

        <div class="custom-color-row">
          <label for="custom-hex">Custom Seed Hex:</label>
          <input
            id="custom-hex"
            type="text"
            class="md3-input"
            placeholder="#10B981"
            bind:value={customHexInput}
            onchange={handleApplyCustomHex}
          />
          <input
            type="color"
            class="color-picker-input"
            value={seedColor}
            oninput={(e) => handleSelectSeed(e.target.value)}
          />
        </div>
      </SectionCard>

      <SectionCard
        title="Theme Mode"
        icon={iconDarkMode}
        description="Select display appearance mode for high contrast, dark, light, or OLED deep blacks."
      >
        <div class="mode-toggles">
          <button
            class="mode-btn"
            class:active={themeMode === 'system'}
            onclick={() => handleSelectMode('system')}
          >
            <span>System</span>
          </button>
          <button
            class="mode-btn"
            class:active={themeMode === 'dark'}
            onclick={() => handleSelectMode('dark')}
          >
            <span>Dark</span>
          </button>
          <button
            class="mode-btn"
            class:active={themeMode === 'amoled'}
            onclick={() => handleSelectMode('amoled')}
          >
            <span>AMOLED Pure Black</span>
          </button>
          <button
            class="mode-btn"
            class:active={themeMode === 'light'}
            onclick={() => handleSelectMode('light')}
          >
            <span>Light</span>
          </button>
        </div>
      </SectionCard>

    <!-- 8. ABOUT & DIAGNOSTICS -->
    {:else if activeCategory === 'about'}
      <div class="panel-header">
        <h2>About & System Diagnostics</h2>
        <span class="subtext">Engine versioning, system dependencies, and environment health checks.</span>
      </div>

      <SectionCard
        title="Application Environment"
        icon={iconInfo}
        description="Host runtime environment details, engine version, and active data directories."
      >
        <div class="info-grid">
          <div class="info-item">
            <span class="info-label">Application:</span>
            <span class="info-val">azdl v0.1.0 (Rust 2024 / Tauri v2 / Svelte 5 MD3)</span>
          </div>
          <div class="info-item">
            <span class="info-label">Data Directory:</span>
            <code class="info-val">{dataDir || 'Default user data path'}</code>
          </div>
          <div class="info-item">
            <span class="info-label">Operating System:</span>
            <span class="info-val">{platform === 'windows' ? 'Microsoft Windows (x86_64)' : 'Linux (Proton / Wine)'}</span>
          </div>
        </div>
      </SectionCard>

      <SectionCard
        title="System Dependencies Health"
        icon={iconAddBox}
        badge="{diagnostics.filter(d => d.available).length}/{diagnostics.length} Available"
        description="Verify availability of runtime dependencies (Wine, Proton, Steam, 7z, UnRAR)."
      >
        <div class="diagnostics-grid">
          {#each diagnostics as diag}
            <div class="diag-card" class:available={diag.available} class:missing={!diag.available}>
              <div class="diag-header">
                {#if diag.available}
                  <Icon icon={iconCheckCircle} class="diag-status-icon success" />
                {:else}
                  <Icon icon={iconError} class="diag-status-icon {diag.required ? 'error' : 'warning'}" />
                {/if}
                <span class="diag-name">{diag.name}</span>
                {#if diag.required}
                  <span class="badge required-badge">Required</span>
                {/if}
              </div>
              <p class="diag-desc">{diag.description}</p>
              {#if diag.path}
                <code class="diag-path">{diag.path}</code>
              {/if}
              {#if diag.version}
                <span class="diag-version">{diag.version}</span>
              {/if}
            </div>
          {/each}
        </div>
      </SectionCard>
    {/if}
  </main>

  <ConfirmModal
    bind:open={confirmDialog.open}
    title={confirmDialog.title}
    message={confirmDialog.message}
    confirmText={confirmDialog.confirmText}
    isDestructive={confirmDialog.isDestructive}
    onConfirm={confirmDialog.action}
  />
</div>

<style>
  .settings-container {
    display: flex;
    height: 100%;
    width: 100%;
    overflow: hidden;
    background: var(--md-sys-color-surface);
  }

  /* SIDENAV */
  .settings-sidenav {
    width: 260px;
    flex-shrink: 0;
    border-right: 1px solid var(--md-sys-color-outline-variant);
    background: var(--md-sys-color-surface-container-low);
    display: flex;
    flex-direction: column;
    padding: 18px 12px;
    gap: 8px;
  }

  .sidenav-header {
    padding: 8px 14px 16px;
  }

  .sidenav-title {
    font-size: 1.15rem;
    font-weight: 700;
    color: var(--md-sys-color-on-surface);
    letter-spacing: -0.01em;
  }

  .sidenav-menu {
    display: flex;
    flex-direction: column;
    gap: 4px;
    overflow-y: auto;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    border-radius: var(--md-sys-shape-full);
    border: none;
    background: transparent;
    color: var(--md-sys-color-on-surface-variant);
    font-size: 0.85rem;
    font-weight: 500;
    cursor: pointer;
    text-align: left;
    transition: background 0.18s cubic-bezier(0.2, 0, 0, 1), color 0.18s cubic-bezier(0.2, 0, 0, 1);
  }

  .nav-item:hover {
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface);
  }

  .nav-item.active {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
    font-weight: 600;
  }

  .nav-item.search-matched:not(.active) {
    background: rgba(var(--md-sys-color-primary-rgb, 16, 185, 129), 0.08);
    color: var(--md-sys-color-primary);
  }

  .match-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--md-sys-color-primary);
    margin-left: auto;
    flex-shrink: 0;
  }

  :global(.nav-icon) {
    width: 20px;
    height: 20px;
    fill: currentColor;
    flex-shrink: 0;
  }

  /* MAIN SETTINGS PANEL */
  .settings-panel {
    flex: 1;
    overflow-y: auto;
    padding: 28px 36px 64px;
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  .panel-header {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .panel-header h2 {
    font-size: 1.55rem;
    font-weight: 700;
    color: var(--md-sys-color-on-surface);
    margin: 0;
  }

  .subtext {
    font-size: 0.85rem;
    color: var(--md-sys-color-on-surface-variant);
  }

  .desc {
    margin: 0;
    font-size: 0.82rem;
    color: var(--md-sys-color-on-surface-variant);
    line-height: 1.45;
  }

  /* FORMS & INPUTS */
  .form-row {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }

  .flex-2 {
    flex: 2;
  }

  .md3-input, .md3-select {
    padding: 10px 14px;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-small);
    color: var(--md-sys-color-on-surface);
    font-size: 0.85rem;
    outline: none;
    transition: none;
  }

  .md3-select {
    padding-right: 36px;
  }

  .md3-input:focus, .md3-select:focus {
    border-color: var(--md-sys-color-primary);
  }

  /* BUTTONS */
  .md3-btn {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 10px 20px;
    border-radius: var(--md-sys-shape-full);
    border: none;
    font-size: 0.85rem;
    font-weight: 500;
    cursor: pointer;
    transition: opacity 0.15s ease, background 0.18s cubic-bezier(0.2, 0, 0, 1);
  }

  .md3-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .md3-btn.primary {
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
  }

  .md3-btn.secondary {
    background: var(--md-sys-color-secondary);
    color: var(--md-sys-color-on-secondary);
  }

  .md3-btn.tonal {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .md3-btn.small {
    padding: 6px 12px;
    font-size: 0.78rem;
  }

  .icon-btn {
    width: 36px;
    height: 36px;
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

  .icon-btn :global(svg) {
    fill: currentColor;
  }

  .icon-btn:hover {
    background: var(--md-sys-color-surface-container-high);
  }

  .icon-btn.tonal {
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface);
  }

  .icon-btn.tonal:hover {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .icon-btn.danger {
    color: var(--md-sys-color-error);
  }

  .icon-btn.danger:hover {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  :global(.btn-icon) {
    width: 18px;
    height: 18px;
    fill: currentColor;
  }

  /* DRIVES LIST */
  .drives-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .drive-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 18px;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-medium);
  }

  .drive-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .drive-label {
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
  }

  .drive-path {
    font-size: 0.75rem;
    color: var(--md-sys-color-on-surface-variant);
  }

  .drive-actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  /* BADGES */
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 0.72rem;
    font-weight: 600;
    padding: 3px 10px;
    border-radius: var(--md-sys-shape-full);
  }

  .default-badge {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
  }

  .required-badge {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }


  /* SUBTABS */
  /* APPEARANCE SWATCHES */
  .seed-swatches {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .swatch-btn {
    width: 40px;
    height: 40px;
    border-radius: 50%;
    border: 2px solid transparent;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: transform 0.18s cubic-bezier(0.2, 0, 0, 1);
  }

  .swatch-btn:hover {
    transform: scale(1.1);
  }

  .swatch-btn.selected {
    border-color: var(--md-sys-color-on-surface);
  }

  .custom-color-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 0.85rem;
  }

  .color-picker-input {
    width: 40px;
    height: 40px;
    border: none;
    border-radius: var(--md-sys-shape-small);
    cursor: pointer;
    background: transparent;
  }

  .mode-toggles {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .mode-btn {
    padding: 10px 18px;
    border-radius: var(--md-sys-shape-full);
    border: 1px solid var(--md-sys-color-outline-variant);
    background: var(--md-sys-color-surface-container-high);
    color: var(--md-sys-color-on-surface);
    font-size: 0.85rem;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.18s ease, color 0.18s ease;
  }

  .mode-btn.active {
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
    border-color: var(--md-sys-color-primary);
  }

  /* SWITCH ROWS & MATERIAL 3 SWITCHES */
  /* DIAGNOSTICS & ABOUT */
  .info-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 12px;
  }

  .info-item {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .info-label {
    font-size: 0.75rem;
    color: var(--md-sys-color-on-surface-variant);
  }

  .info-val {
    font-size: 0.85rem;
    font-weight: 500;
    color: var(--md-sys-color-on-surface);
  }

  .diagnostics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 12px;
  }

  .diag-card {
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-medium);
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .diag-header {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  :global(.diag-status-icon) {
    width: 18px;
    height: 18px;
    flex-shrink: 0;
  }

  :global(.diag-status-icon.success) {
    fill: var(--md-sys-color-primary);
  }

  :global(.diag-status-icon.warning) {
    fill: var(--md-sys-color-tertiary);
  }

  :global(.diag-status-icon.error) {
    fill: var(--md-sys-color-error);
  }

  .diag-name {
    font-weight: 600;
    font-size: 0.88rem;
    color: var(--md-sys-color-on-surface);
  }

  .diag-desc {
    font-size: 0.78rem;
    color: var(--md-sys-color-on-surface-variant);
    margin: 0;
    line-height: 1.4;
  }

  .diag-path {
    font-size: 0.72rem;
    color: var(--md-sys-color-on-surface-variant);
  }

  .diag-version {
    font-size: 0.72rem;
    color: var(--md-sys-color-on-surface-variant);
    font-style: italic;
  }

  /* Storage & Drives Styling */
  .add-drive-box {
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-medium);
    padding: 16px;
    margin-top: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .add-drive-box h4 {
    font-size: 0.92rem;
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
    margin: 0;
  }

  .games-management-table {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .game-management-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-medium);
    padding: 12px 16px;
    flex-wrap: wrap;
  }

  .game-main-info {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 200px;
    flex: 1;
  }

  .game-row-title {
    font-weight: 600;
    font-size: 0.92rem;
    color: var(--md-sys-color-on-surface);
  }

  .game-row-path {
    font-size: 0.76rem;
    color: var(--md-sys-color-on-surface-variant);
    background: var(--md-sys-color-surface-container-high);
    padding: 2px 6px;
    border-radius: var(--md-sys-shape-small);
    word-break: break-all;
  }

  .game-move-controls {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .runner-select {
    display: flex;
    width: 100%;
  }

  .small-select {
    min-width: 200px;
    max-width: 300px;
    font-size: 0.82rem;
  }

  .prefix-inline-note {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    background: var(--md-sys-color-surface-container-high);
    border-radius: var(--md-sys-shape-medium);
    padding: 12px 14px;
    font-size: 0.82rem;
    color: var(--md-sys-color-on-surface-variant);
    line-height: 1.45;
  }

  .prefix-inline-note :global(svg) {
    flex-shrink: 0;
    margin-top: 2px;
    fill: var(--md-sys-color-primary);
  }

</style>
