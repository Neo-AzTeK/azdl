<script>
  import { Icon } from 'm3-svelte';
  import iconDelete from '@ktibow/iconset-material-symbols/delete-outline';
  import iconError from '@ktibow/iconset-material-symbols/error-outline';
  import iconLibraryAdd from '@ktibow/iconset-material-symbols/library-add-outline';
  import iconVideogameAsset from '@ktibow/iconset-material-symbols/videogame-asset-outline';
  import GameCard from '../components/GameCard.svelte';
  import ImportGameModal from '../components/ImportGameModal.svelte';
  import ConfirmModal from '../components/ConfirmModal.svelte';
  import { runAction } from '../api.js';
  import { notify } from '../notifications.js';

  let {
    library = [],
    pending = [],
    searchQuery = '',
    onSelectGame,
    onLaunchGame,
    onStopGame,
    onRefresh,
    onBrowseCatalog = null,
  } = $props();

  let modalOpen = $state(false);
  let selectedPendingItem = $state(null);
  let itemToRemove = $state(null);
  let showRemoveModal = $state(false);

  let filteredGames = $derived.by(() => {
    const query = searchQuery.trim().toLowerCase();
    if (!query) return library;
    return library.filter((game) =>
      (game.title || '').toLowerCase().includes(query) ||
      (game.id || '').toLowerCase().includes(query)
    );
  });

  function handlePendingAdd(item) {
    selectedPendingItem = item;
    modalOpen = true;
  }

  function promptRemovePending(item) {
    itemToRemove = item;
    showRemoveModal = true;
  }

  async function handleConfirmRemove() {
    if (!itemToRemove) return;
    try {
      await runAction({
        action: 'pending-remove',
        id: itemToRemove.id,
        install_dir: itemToRemove.directory,
      });
      if (onRefresh) await onRefresh();
    } catch (err) {
      notify(`Failed to remove directory: ${err}`, 'error');
    } finally {
      itemToRemove = null;
      showRemoveModal = false;
    }
  }
</script>

<div class="library-view">
  <!-- Sub-header Controls -->
  <div class="library-toolbar">
    <div class="toolbar-left">
      <span class="game-count-label">
        {filteredGames.length} Game{filteredGames.length === 1 ? '' : 's'}
      </span>
    </div>
  </div>

  <!-- Pending Unfinished Installs Banner -->
  {#if pending.length > 0}
    <div class="pending-box">
      <div class="pending-header">
        <Icon icon={iconError} size={16} />
        <span>{pending.length} Unfinished or Portable Game Directory Detected:</span>
      </div>
      <div class="pending-list">
        {#each pending as item}
          <div class="pending-item">
            <span class="pending-id">{item.id}</span>
            <span class="pending-kind">({item.kind})</span>
            <button class="finish-btn" onclick={() => handlePendingAdd(item)}>
              Select Executable
            </button>
            <button
              class="icon-btn-inline danger"
              title="Remove Directory"
              onclick={() => promptRemovePending(item)}
            >
              <Icon icon={iconDelete} size={14} />
            </button>
          </div>
        {/each}
      </div>
    </div>
  {/if}

  <!-- Games Grid -->
  <div class="games-grid">
    {#if filteredGames.length === 0}
      <div class="empty-state">
        <Icon icon={iconVideogameAsset} size={64} />
        <span class="empty-text">No games found in your library.</span>
        {#if onBrowseCatalog}
          <button class="import-action-btn empty-cta" onclick={onBrowseCatalog}>
            <Icon icon={iconLibraryAdd} size={16} />
            <span>Explore Catalog</span>
          </button>
        {/if}
      </div>
    {:else}
      {#each filteredGames as game (game.id)}
        <GameCard
          {game}
          onSelect={onSelectGame}
          onLaunch={onLaunchGame}
          onStop={onStopGame}
        />
      {/each}
    {/if}
  </div>

  <ImportGameModal
    bind:open={modalOpen}
    pendingItem={selectedPendingItem}
    onSuccess={() => {
      if (onRefresh) onRefresh();
    }}
  />

  <ConfirmModal
    bind:open={showRemoveModal}
    title="Remove Directory"
    message={itemToRemove ? `Are you sure you want to permanently delete the unfinished or portable directory "${itemToRemove.id}"? All files will be erased.` : ''}
    confirmText="Delete Directory"
    isDestructive={true}
    onConfirm={handleConfirmRemove}
    onCancel={() => { itemToRemove = null; }}
  />
</div>

<style>
  .library-view {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow-y: auto;
    padding: 16px 24px;
    gap: 16px;
  }

  .library-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }

  .toolbar-left {
    display: flex;
    align-items: center;
    gap: 16px;
  }

  .game-count-label {
    font-size: 14px;
    font-weight: 700;
    color: var(--md-sys-color-on-surface);
  }

  .import-action-btn {
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
    border: none;
    border-radius: 10px;
    padding: 8px 14px;
    font-size: 12px;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
    transition: opacity 150ms ease;
  }

  .import-action-btn:hover {
    opacity: 0.9;
  }

  .pending-box {
    background: var(--md-sys-color-tertiary-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-medium);
    padding: 12px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .pending-header {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--md-sys-color-on-tertiary-container);
  }

  .pending-list {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .pending-item {
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    padding: 4px 10px;
    border-radius: 6px;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }

  .pending-id {
    font-weight: 600;
  }

  .pending-kind {
    color: var(--md-sys-color-on-surface-variant);
    font-size: 11px;
  }

  .finish-btn {
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
    border: none;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 600;
    cursor: pointer;
  }

  .icon-btn-inline {
    background: transparent;
    border: none;
    padding: 2px 4px;
    border-radius: 4px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    color: var(--md-sys-color-on-surface-variant);
    transition: background 150ms ease, color 150ms ease;
  }

  .icon-btn-inline:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  .icon-btn-inline.danger {
    color: var(--md-sys-color-error);
  }

  .icon-btn-inline.danger:hover {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  .games-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 18px;
    width: 100%;
  }

  .empty-state {
    grid-column: 1 / -1;
    padding: 64px 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .empty-text {
    font-size: 14px;
    font-weight: 500;
  }

  .empty-cta {
    margin-top: 8px;
  }
</style>
