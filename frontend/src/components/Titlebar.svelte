<script>
  import { Icon } from 'm3-svelte';
  import iconClose from '@ktibow/iconset-material-symbols/close';
  import iconCropSquare from '@ktibow/iconset-material-symbols/crop-square-outline';
  import iconRemove from '@ktibow/iconset-material-symbols/remove';
  import iconSearch from '@ktibow/iconset-material-symbols/search';
  import iconVideogameAsset from '@ktibow/iconset-material-symbols/videogame-asset-outline';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { windowMinimize, windowToggleMaximize, windowClose, stopGame, isDesktop } from '../api.js';

  let {
    searchQuery = $bindable(''),
    activeTab = 'library',
    runningGames = {},
    onStopGame = null,
  } = $props();

  let searchPlaceholder = $derived.by(() => {
    switch (activeTab) {
      case 'library':
        return 'Search installed library & pending games...';
      case 'catalog':
        return 'Search Steam games by title or App ID...';
      case 'settings':
        return 'Search settings & options...';
      case 'game':
        return 'Search game media & options...';
      default:
        return 'Search azdl...';
    }
  });

  function handleDrag(e) {
    if (e.target.closest('button') || e.target.closest('input')) return;
    if (isDesktop() && e.buttons === 1) {
      try {
        getCurrentWindow().startDragging();
      } catch {}
    }
  }
</script>

<header class="titlebar" role="toolbar" aria-label="Window titlebar" tabindex="-1" data-tauri-drag-region onmousedown={handleDrag}>
  <!-- Window Drag / Branding Section -->
  <div class="brand" data-tauri-drag-region>
    <div class="brand-icon">
      <Icon icon={iconVideogameAsset} size={18} />
    </div>
    <span class="brand-title" data-tauri-drag-region>azdl</span>
  </div>

  <!-- Center Area: Global Search & Active Game Pill -->
  <div class="center-area" data-tauri-drag-region>
    <div class="search-box">
      <Icon icon={iconSearch} size={16} class="search-icon" />
      <input
        type="text"
        placeholder={searchPlaceholder}
        bind:value={searchQuery}
      />
      {#if searchQuery}
        <button class="clear-btn" onclick={() => (searchQuery = '')} title="Clear search" aria-label="Clear search">
          <Icon icon={iconClose} size={16} />
        </button>
      {/if}
    </div>

  </div>

  <!-- Right Area: Window Controls (Minimize, Maximize, Close) -->
  <div class="window-controls">
    <button class="win-btn win-minimize" onclick={windowMinimize} title="Minimize">
      <Icon icon={iconRemove} size={10} />
    </button>
    <button class="win-btn win-maximize" onclick={windowToggleMaximize} title="Maximize">
      <Icon icon={iconCropSquare} size={10} />
    </button>
    <button class="win-btn win-close" onclick={windowClose} title="Close">
      <Icon icon={iconClose} size={10} />
    </button>
  </div>
</header>

<style>
  .titlebar {
    height: 48px;
    min-height: 48px;
    background: var(--md-sys-color-surface-container-low);
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 0 0 16px;
    user-select: none;
    -webkit-user-select: none;
    border-bottom: 1px solid var(--md-sys-color-outline-variant);
    z-index: 100;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: default;
  }

  .brand-icon {
    color: var(--md-sys-color-primary);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .brand-title {
    font-size: 15px;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: var(--md-sys-color-on-surface);
  }

  .center-area {
    display: flex;
    align-items: center;
    gap: 16px;
    flex: 1;
    max-width: 680px;
    margin: 0 24px;
  }

  .search-box {
    position: relative;
    display: flex;
    align-items: center;
    flex: 1;
    background: var(--md-sys-color-surface-container-highest);
    border: 1px solid transparent;
    border-radius: 20px;
    padding: 0 12px;
    height: 32px;
    transition: none;
  }

  .search-box:focus-within {
    border-color: var(--md-sys-color-primary);
    background: var(--md-sys-color-surface);
    box-shadow: none;
  }

  :global(.search-icon) {
    color: var(--md-sys-color-on-surface-variant);
    margin-right: 6px;
    flex-shrink: 0;
  }

  .search-box input {
    background: transparent;
    border: none;
    outline: none;
    color: var(--md-sys-color-on-surface);
    font-size: 13px;
    width: 100%;
    font-family: inherit;
  }

  .search-box input::placeholder {
    color: var(--md-sys-color-on-surface-variant);
    opacity: 0.8;
  }

  .clear-btn {
    background: none;
    border: none;
    color: var(--md-sys-color-on-surface-variant);
    font-size: 11px;
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 50%;
  }

  .clear-btn:hover {
    color: var(--md-sys-color-on-surface);
  }


  .window-controls {
    display: flex;
    align-items: center;
    height: 100%;
    -webkit-app-region: no-drag;
  }

  .win-btn {
    width: 44px;
    height: 100%;
    background: transparent;
    border: none;
    color: var(--md-sys-color-on-surface-variant);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background-color 150ms ease, color 150ms ease;
  }

  .win-btn:hover {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
  }

  .win-btn.win-close:hover {
    background: var(--md-sys-color-error);
    color: var(--md-sys-color-on-error);
  }
</style>
