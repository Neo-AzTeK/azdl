<script>
  import { PRESET_SEEDS } from '../theme.js';
  import { displayPath } from '../paths.js';
  import { Switch } from 'm3-svelte';
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
    activeCategory = $bindable('storage'), // 'storage' | 'launch' | 'appearance' | 'patches' | 'about'
    runners = [],
    dataDir = '',
    platform = 'linux',
    settings = null,
    diagnostics = [],
    library = [],
    searchQuery = '',
    onThemeChange,
    onRefresh,
    onShowToast,
  } = $props();

  // Local working copy of settings
  let localSettings = $state({
    storage_drives: [],
    downloads_dir: '',
    default_runner: 'auto',
    global_launch_args: '',
    default_proton_id: '',
    max_concurrent_downloads: 3,
    auto_steamless_unpack: true,
    auto_online_fix_overlay: true,
    auto_goldberg_sync: true,
    auto_dlc_patch: true,
    auto_ea_heal: true,
    auto_game_patch: true,
    seed_color: seedColor,
    theme_mode: themeMode,
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
    { id: 'storage', label: 'Storage & Drives', icon: 'M2 20h20v-4H2v4zm2-3h2v2H4v-2zM2 4v4h20V4H2zm4 3H4V5h2v2zm-4 7h20v-4H2v4zm2-3h2v2H4v-2z', keywords: ['storage', 'drive', 'disk', 'folder', 'migrate', 'path', 'games'] },
    { id: 'launch', label: platform === 'windows' ? 'Launch Options' : 'Launch & Runners', icon: 'M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 14.5v-9l6 4.5-6 4.5z', keywords: ['launch', 'proton', 'wine', 'runner', 'ge', 'arguments', 'dxvk', 'prefix'] },
    { id: 'appearance', label: 'Material You Theming', icon: 'M12 3c-4.97 0-9 4.03-9 9 0 2.12.74 4.07 1.97 5.61L4.35 19.4c-.39.39-.39 1.02 0 1.41.39.39 1.02.39 1.41 0l1.9-1.9C9.23 19.64 10.57 20 12 20c4.97 0 9-4.03 9-9s-4.03-9-9-9zm0 15c-3.31 0-6-2.69-6-6s2.69-6 6-6 6 2.69 6 6-2.69 6-6 6z', keywords: ['theme', 'appearance', 'material', 'color', 'seed', 'dark', 'light'] },
    { id: 'patches', label: 'Patches', icon: 'M18 8h-1V6c0-2.76-2.24-5-5-5S7 3.24 7 6v2H6c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V10c0-1.1-.9-2-2-2zm-6 9c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2zm3.1-9H8.9V6c0-1.71 1.39-3.1 3.1-3.1 1.71 0 3.1 1.39 3.1 3.1v2z', keywords: ['patch', 'patches', 'steamless', 'onlinefix', 'overlay', 'drm', 'steamstub', 'fix', 'goldberg', 'dlc', 'ea'] },
    { id: 'about', label: 'About & Diagnostics', icon: 'M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z', keywords: ['about', 'diagnostics', 'doctor', 'version', 'system', 'bwrap', 'unrar', 'bsdtar'] },
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
  async function handleDownloadProton() {
    isDownloadingProton = true;
    notify('Downloading latest Proton-GE release...', 'info');
    try {
      const res = await downloadProtonGE();
      notify(`Proton-GE installed: ${res?.runner || 'Done'}`, 'info');
      if (onRefresh) await onRefresh();
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
          <svg class="nav-icon" viewBox="0 0 24 24"><path d={cat.icon} /></svg>
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
        icon="M2 20h20v-4H2v4zm2-3h2v2H4v-2zM2 4v4h20V4H2zm4 3H4V5h2v2zm-4 7h20v-4H2v4zm2-3h2v2H4v-2z"
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
                      <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor"><path d="M12 17.27L18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z"/></svg>
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
                    <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z"/></svg>
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
              <svg class="btn-icon" viewBox="0 0 24 24"><path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/></svg>
              Add Drive
            </button>
          </div>
        </div>
      </SectionCard>

      <!-- CONTAINER 2: GAMES (TO MANAGE GAME INSTALLATIONS & EASY MOVING) -->
      <SectionCard
        title="Games (Manage Game Installations)"
        icon="M21 6H3c-1.1 0-2 .9-2 2v8c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm-10 7H8v3H6v-3H3v-2h3V8h2v3h3v2zm4.5 2c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm4-3c-.83 0-1.5-.67-1.5-1.5S18.67 9 19.5 9s1.5.67 1.5 1.5-.67 1.5-1.5 1.5z"
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
                  <select
                    class="md3-select small-select"
                    bind:value={individualTargetDrives[game.id]}
                  >
                    <option value="" disabled selected>Select destination drive...</option>
                    {#each localSettings.storage_drives as d}
                      <option value={d.id}>{d.label} ({displayPath(d.path)})</option>
                    {/each}
                  </select>

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
                    <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor">
                      <path d="M20 6h-8l-2-2H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm0 12H4V8h16v10z"/>
                    </svg>
                  </button>

                  <button
                    class="icon-btn danger"
                    onclick={() => requestUninstallGame(game.id, game.title)}
                    title="Uninstall game"
                  >
                    <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z"/></svg>
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </SectionCard>

    <!-- 2. LAUNCH & RUNNERS (CLEAR & UNAMBIGUOUS) -->
    {:else if activeCategory === 'launch'}
      <div class="panel-header">
        <div class="panel-header-row">
          <div>
            <h2>{platform === 'windows' ? 'Launch Options' : 'Launch & Compatibility Runners'}</h2>
            <span class="subtext">
              {platform === 'windows'
                ? 'Configure global launch parameters and execution options.'
                : 'Orchestrate runtime compatibility for Windows executables and game prefixes.'}
            </span>
          </div>
          <div class="platform-chip" title="Detected Host: {platform}">
            {#if platform === 'linux'}
              <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                <path d="M12 2a4 4 0 0 0-4 4c0 .77.22 1.48.58 2.09C6.43 9.42 5 11.54 5 14c0 3.31 2.69 6 6 6h2c3.31 0 6-2.69 6-6 0-2.46-1.43-4.58-3.58-5.91.36-.61.58-1.32.58-2.09a4 4 0 0 0-4-4z"/>
              </svg>
              <span>Linux</span>
            {:else}
              <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
                <path d="M3 5.48L10.37 4.46V11.53H3V5.48ZM10.37 12.44V19.54L3 18.52V12.44H10.37ZM11.49 4.31L21 3V11.53H11.49V4.31ZM21 12.44V21L11.49 19.69V12.44H21Z"/>
              </svg>
              <span>Windows</span>
            {/if}
          </div>
        </div>
      </div>

      {#if platform === 'linux'}
        <!-- CARD 1: WINDOWS COMPATIBILITY RUNNER WITH INTEGRATED PREFIX ISOLATION (LINUX ONLY) -->
        <SectionCard
          title="Windows Compatibility Runner (Wine / Proton)"
          icon="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 14.5v-9l6 4.5-6 4.5z"
          description="Select which runner executes Windows PE binaries (.exe) and Steam manifests on your system:"
        >
          <div class="form-row">
            <select
              class="md3-select flex-2"
              bind:value={localSettings.default_runner}
              onchange={saveLocalSettings}
            >
              <option value="auto">Proton Auto (Discovers latest Proton-GE or Valve Proton — Recommended)</option>
              {#each runners.filter(r => r.runner_type === 'proton') as p}
                <option value={p.id}>{p.name}</option>
              {/each}
              <option value="wine">System Wine</option>
            </select>

            <button class="md3-btn primary" onclick={handleDownloadProton} disabled={isDownloadingProton}>
              <span>{isDownloadingProton ? 'Downloading...' : 'Download Latest Proton-GE'}</span>
            </button>
          </div>

          <div class="prefix-inline-note">
            <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
              <path d="M11 17h2v-6h-2v6zm1-15C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8zm-1-13h2v2h-2zm0 0"/>
            </svg>
            <span>
              <strong>Dedicated Prefix Isolation:</strong> Windows games run inside isolated prefixes (<code>~/.local/share/azdl/prefixes/&lt;game_id&gt;</code>) to avoid library collisions, isolate saves, and allow per-game <code>WINEDLLOVERRIDES</code> staging for multiplayer fix DLLs with automated rollback.
            </span>
          </div>
        </SectionCard>
      {/if}

      <!-- CARD 2: GLOBAL LAUNCH ARGUMENTS -->
      <SectionCard
        title="Global Launch Arguments"
        icon="M9.4 16.6L4.8 12l4.6-4.6L8 6l-6 6 6 6 1.4-1.4zm5.2 0l4.6-4.6-4.6-4.6L16 6l6 6-6 6-1.4-1.4z"
        description="Parameters appended to every game invocation (e.g. gamemoderun %command%, DXVK_HUD=1):"
      >
        <div class="form-row">
          <input
            type="text"
            class="md3-input flex-2"
            placeholder="e.g. --fullscreen -novid"
            bind:value={localSettings.global_launch_args}
          />
          <button class="md3-btn secondary" onclick={saveLocalSettings}>Save Arguments</button>
        </div>
      </SectionCard>

    {:else if activeCategory === 'appearance'}
      <div class="panel-header">
        <h2>Material You Personalization</h2>
        <span class="subtext">Dynamic algorithmic palette generation and theme modes.</span>
      </div>

      <SectionCard
        title="Seed Color Palette"
        icon="M12 3c-4.97 0-9 4.03-9 9 0 2.12.74 4.07 1.97 5.61L4.35 19.4c-.39.39-.39 1.02 0 1.41.39.39 1.02.39 1.41 0l1.9-1.9C9.22 19.62 10.56 20 12 20c4.97 0 9-4.03 9-9s-4.03-9-9-9zm0 15c-3.31 0-6-2.69-6-6s2.69-6 6-6 6 2.69 6 6-2.69 6-6 6z"
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
                <svg viewBox="0 0 24 24" width="16" height="16" fill="white">
                  <path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/>
                </svg>
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
        icon="M12 3a9 9 0 1 0 9 9c0-.46-.04-.92-.1-1.36a5.389 5.389 0 0 1-4.4 2.26 5.403 5.403 0 0 1-5.4-5.4c0-1.81.89-3.42 2.26-4.4-.44-.06-.9-.1-1.36-.1z"
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

    <!-- 7. INTEGRATIONS & FIXES -->
    {:else if activeCategory === 'integrations'}
    <!-- 7. PATCHES -->
    {:else if activeCategory === 'patches'}
      <div class="panel-header">
        <h2>Patches</h2>
        <span class="subtext">Configure global automated patching, DRM stripping, and runtime hooks.</span>
      </div>

      <SectionCard
        title="Automated Runtime Patches & DRM Stripping"
        icon="M18 8h-1V6c0-2.76-2.24-5-5-5S7 3.24 7 6v2H6c-1.1 0-2 .9-2 2v10c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V10c0-1.1-.9-2-2-2zm-6 9c-1.1 0-2-.9-2-2s.9-2 2-2 2 .9 2 2-.9 2-2 2zm3.1-9H8.9V6c0-1.71 1.39-3.1 3.1-3.1 1.71 0 3.1 1.39 3.1 3.1v2z"
        description="Global automated patching rules executed during game import and launch lifecycle:"
      >
        <div class="switch-row">
          <div>
            <strong>SteamStub Auto-Unpack (Steamless)</strong>
            <p class="desc">Automatically strip SteamStub DRM wrappers on import, preserving <code>.steamstub.bak</code> backups.</p>
          </div>
          <label class="switch-container" aria-label="SteamStub Auto-Unpack">
            <Switch
              bind:checked={localSettings.auto_steamless_unpack}
              onchange={saveLocalSettings}
            />
          </label>
        </div>

        <div class="switch-row">
          <div>
            <strong>Auto-Enable Online-Fix / SteamFix Overlays</strong>
            <p class="desc">Set Wine DLL overrides (<code>WINEDLLOVERRIDES="OnlineFix64=n,b"</code>) and preserve clean unpatched states via <code>.arachnel-off</code> renames.</p>
          </div>
          <label class="switch-container" aria-label="Auto-Enable Online-Fix Overlays">
            <Switch
              bind:checked={localSettings.auto_online_fix_overlay}
              onchange={saveLocalSettings}
            />
          </label>
        </div>

        <div class="switch-row">
          <div>
            <strong>Goldberg Steam Emulator Auto-Sync</strong>
            <p class="desc">Automatically generate <code>steam_settings/</code> configurations (<code>DLC.txt</code>, <code>configs.app.ini</code> with <code>unlock_all=1</code>) and deploy modern emulator binaries for compatible games.</p>
          </div>
          <label class="switch-container" aria-label="Goldberg Steam Emulator Auto-Sync">
            <Switch
              bind:checked={localSettings.auto_goldberg_sync}
              onchange={saveLocalSettings}
            />
          </label>
        </div>

        <div class="switch-row">
          <div>
            <strong>Steamworks DLC Entitlement Auto-Bypass</strong>
            <p class="desc">Automatically patch Steamworks <code>BIsDlcInstalled</code> entitlement checks in <code>steam_api64.dll</code> and GodotSteam DLLs on launch.</p>
          </div>
          <label class="switch-container" aria-label="Steamworks DLC Entitlement Auto-Bypass">
            <Switch
              bind:checked={localSettings.auto_dlc_patch}
              onchange={saveLocalSettings}
            />
          </label>
        </div>

        <div class="switch-row">
          <div>
            <strong>EA Desktop / Origin Activation Repair</strong>
            <p class="desc">Automatically bypass EA Desktop / Origin activation checks in Frostbite and EA game binaries (<code>Activation64.dll</code>) on launch.</p>
          </div>
          <label class="switch-container" aria-label="EA Desktop / Origin Activation Repair">
            <Switch
              bind:checked={localSettings.auto_ea_heal}
              onchange={saveLocalSettings}
            />
          </label>
        </div>

        <div class="switch-row">
          <div>
            <strong>Standalone Game Patch Auto-Mount</strong>
            <p class="desc">Automatically enable and mount staged standalone game patches and community modifications on launch.</p>
          </div>
          <label class="switch-container" aria-label="Standalone Game Patch Auto-Mount">
            <Switch
              bind:checked={localSettings.auto_game_patch}
              onchange={saveLocalSettings}
            />
          </label>
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
        icon="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-6h2v6zm0-8h-2V7h2v2z"
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
        icon="M19 3H5c-1.1 0-1.99.9-1.99 2L3 19c0 1.1.9 2 2 2h14c1.1 0 2-.9 2-2V5c0-1.1-.9-2-2-2zm-1 11h-4v4h-4v-4H6v-4h4V6h4v4h4v4z"
        badge="{diagnostics.filter(d => d.available).length}/{diagnostics.length} Available"
        description="Verify availability of runtime dependencies (Wine, Proton, Steam, 7z, UnRAR)."
      >
        <div class="diagnostics-grid">
          {#each diagnostics as diag}
            <div class="diag-card" class:available={diag.available} class:missing={!diag.available}>
              <div class="diag-header">
                {#if diag.available}
                  <svg class="diag-status-icon success" viewBox="0 0 24 24"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z"/></svg>
                {:else}
                  <svg class="diag-status-icon {diag.required ? 'error' : 'warning'}" viewBox="0 0 24 24"><path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z"/></svg>
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

  .nav-icon {
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

  .md3-btn.danger {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
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

  .icon-btn svg {
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

  .btn-icon {
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

  .recommended-badge {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
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
  .switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 0;
    border-bottom: 1px solid var(--md-sys-color-outline-variant);
    gap: 16px;
  }

  .switch-row:last-child {
    border-bottom: none;
  }

  .switch-container {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    flex-shrink: 0;
  }

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

  .diag-status-icon {
    width: 18px;
    height: 18px;
    flex-shrink: 0;
  }

  .diag-status-icon.success {
    fill: var(--md-sys-color-primary);
  }

  .diag-status-icon.warning {
    fill: var(--md-sys-color-tertiary);
  }

  .diag-status-icon.error {
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

  .small-select {
    min-width: 200px;
    max-width: 300px;
    font-size: 0.82rem;
  }

  .panel-header-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }

  .platform-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 14px;
    border-radius: var(--md-sys-shape-full);
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
    font-size: 0.8rem;
    font-weight: 600;
  }

  .platform-chip svg {
    fill: currentColor;
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

  .prefix-inline-note svg {
    flex-shrink: 0;
    margin-top: 2px;
    fill: var(--md-sys-color-primary);
  }

</style>
