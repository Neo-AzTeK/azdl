<script>
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
        return 'Search settings, plugins & options...';
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
      <svg viewBox="0 0 24 24" fill="currentColor" width="18" height="18">
        <path d="M21 6H3c-1.1 0-2 .9-2 2v8c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm-10 7H8v3H6v-3H3v-2h3V8h2v3h3v2zm4.5 2c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm4-3c-.83 0-1.5-.67-1.5-1.5S18.67 9 19.5 9s1.5.67 1.5 1.5-.67 1.5-1.5 1.5z"/>
      </svg>
    </div>
    <span class="brand-title" data-tauri-drag-region>azdl</span>
  </div>

  <!-- Center Area: Global Search & Active Game Pill -->
  <div class="center-area" data-tauri-drag-region>
    <div class="search-box">
      <svg class="search-icon" viewBox="0 0 24 24" width="16" height="16" fill="currentColor">
        <path d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0 0 16 9.5 6.5 6.5 0 1 0 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z"/>
      </svg>
      <input
        type="text"
        placeholder={searchPlaceholder}
        bind:value={searchQuery}
      />
      {#if searchQuery}
        <button class="clear-btn" onclick={() => (searchQuery = '')} title="Clear search">
          ✕
        </button>
      {/if}
    </div>

  </div>

  <!-- Right Area: Window Controls (Minimize, Maximize, Close) -->
  <div class="window-controls">
    <button class="win-btn win-minimize" onclick={windowMinimize} title="Minimize">
      <svg viewBox="0 0 12 12" width="10" height="10" stroke="currentColor" stroke-width="1.2">
        <line x1="1" y1="6" x2="11" y2="6" />
      </svg>
    </button>
    <button class="win-btn win-maximize" onclick={windowToggleMaximize} title="Maximize">
      <svg viewBox="0 0 12 12" width="10" height="10" stroke="currentColor" stroke-width="1.2" fill="none">
        <rect x="2" y="2" width="8" height="8" rx="1" />
      </svg>
    </button>
    <button class="win-btn win-close" onclick={windowClose} title="Close">
      <svg viewBox="0 0 12 12" width="10" height="10" stroke="currentColor" stroke-width="1.2">
        <line x1="2" y1="2" x2="10" y2="10" />
        <line x1="10" y1="2" x2="2" y2="10" />
      </svg>
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

  .search-icon {
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
