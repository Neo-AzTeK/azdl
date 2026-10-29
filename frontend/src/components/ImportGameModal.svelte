<script>
  import { Icon } from 'm3-svelte';
  import iconArrowDropDown from '@ktibow/iconset-material-symbols/arrow-drop-down';
  import { runAction, discoverExecutables } from '../api.js';
  import Modal from './Modal.svelte';
  import ConfirmModal from './ConfirmModal.svelte';

  let {
    open = $bindable(false),
    pendingItem = null,
    onSuccess = null,
  } = $props();

  let id = $state('');
  let installDir = $state('');
  let selectedExe = $state('');
  let customExeText = $state('');
  let candidates = $state([]);
  let loadingCandidates = $state(false);
  let submitting = $state(false);
  let errorMsg = $state('');
  let showDeleteConfirm = $state(false);

  $effect(() => {
    if (open) {
      errorMsg = '';
      submitting = false;
      showDeleteConfirm = false;
      candidates = [];
      selectedExe = '';
      customExeText = '';

      if (pendingItem) {
        id = pendingItem.id || '';
        installDir = pendingItem.directory || '';
        loadCandidates(pendingItem.id);
      } else {
        id = '';
        installDir = '';
      }
    }
  });

  async function loadCandidates(gameId) {
    loadingCandidates = true;
    try {
      const res = await discoverExecutables(gameId);
      if (Array.isArray(res) && res.length > 0) {
        candidates = res;
        selectedExe = res[0].relative_path || res[0].path || 'game.exe';
      } else {
        candidates = [];
        selectedExe = '__custom__';
        customExeText = 'game.exe';
      }
    } catch (e) {
      console.warn('Failed discovering executables:', e);
      candidates = [];
      selectedExe = '__custom__';
      customExeText = 'game.exe';
    } finally {
      loadingCandidates = false;
    }
  }

  async function handleSubmit(e) {
    e.preventDefault();
    errorMsg = '';

    if (!pendingItem) {
      errorMsg = 'No pending game item selected.';
      return;
    }

    const finalExe = selectedExe === '__custom__' ? customExeText.trim() : selectedExe.trim();
    if (!finalExe) {
      errorMsg = 'Please select or specify an executable.';
      return;
    }

    submitting = true;
    try {
      await runAction({
        action: 'pending-add',
        id: pendingItem.id,
        title: pendingItem.id,
        install_dir: pendingItem.directory,
        executable: finalExe,
      });

      open = false;
      if (onSuccess) onSuccess();
    } catch (err) {
      errorMsg = String(err);
    } finally {
      submitting = false;
    }
  }

  async function handleDeleteDirectory() {
    if (!pendingItem) return;
    submitting = true;
    try {
      await runAction({
        action: 'pending-remove',
        id: pendingItem.id,
        install_dir: pendingItem.directory,
      });
      open = false;
      if (onSuccess) onSuccess();
    } catch (err) {
      errorMsg = String(err);
    } finally {
      submitting = false;
    }
  }

  function handleClose() {
    if (!submitting) {
      open = false;
    }
  }
</script>

<Modal
  {open}
  title="Configure Executable"
  subtitle="Select main executable for detected directory"
  maxWidth="520px"
  onClose={handleClose}
>
  {#snippet children()}
    <form id="import-form" onsubmit={handleSubmit} class="modal-form">
      {#if errorMsg}
        <div class="error-banner">
          <span>{errorMsg}</span>
        </div>
      {/if}

      <div class="form-group">
        <label for="game-directory">Game Directory</label>
        <div id="game-directory" class="directory-pill">
          <span class="dir-name">{id}</span>
        </div>
      </div>

      <div class="form-group">
        <label for="discovered-path">Storage Path</label>
        <input
          id="discovered-path"
          type="text"
          class="md3-input"
          value={installDir}
          readonly
          disabled
        />
      </div>

      <div class="form-group">
        <div class="label-row">
          <label for="exe-select">Main Executable</label>
          {#if loadingCandidates}
            <span class="scanning-indicator">Scanning executables...</span>
          {/if}
        </div>

        {#if candidates.length > 0}
          <span class="select-field">
            <select
              id="exe-select"
              class="md3-select"
              bind:value={selectedExe}
              disabled={submitting}
            >
              {#each candidates as cand}
                {@const val = cand.relative_path || cand.path}
                <option value={val}>
                  {val} {cand.score ? `(Score: ${cand.score})` : ''}
                </option>
              {/each}
              <option value="__custom__">Custom path...</option>
            </select>
            <Icon icon={iconArrowDropDown} size={20} class="select-chevron" />
          </span>
        {/if}

        {#if selectedExe === '__custom__' || candidates.length === 0}
          <input
            id="executable"
            type="text"
            class="md3-input"
            placeholder="e.g. game.exe or bin/game.exe"
            bind:value={customExeText}
            disabled={submitting}
            required
          />
        {/if}
      </div>
    </form>
  {/snippet}

  {#snippet actions()}
    <div class="actions-container">
      <button
        type="button"
        class="md3-btn danger"
        onclick={() => (showDeleteConfirm = true)}
        disabled={submitting}
      >
        Remove Directory
      </button>

      <div class="actions-right">
        <button
          type="button"
          class="md3-btn text"
          onclick={handleClose}
          disabled={submitting}
        >
          Cancel
        </button>
        <button
          type="submit"
          form="import-form"
          class="md3-btn primary"
          disabled={submitting || loadingCandidates}
        >
          {submitting ? 'Saving...' : 'Add Game'}
        </button>
      </div>
    </div>
  {/snippet}
</Modal>

<ConfirmModal
  bind:open={showDeleteConfirm}
  title="Remove Directory"
  message={`Are you sure you want to delete the directory "${id}"? All files inside it will be permanently removed.`}
  confirmText="Delete Directory"
  isDestructive={true}
  onConfirm={handleDeleteDirectory}
/>

<style>
  .modal-form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .label-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  label {
    font-size: 12px;
    font-weight: 500;
    color: var(--md-sys-color-on-surface-variant);
  }

  .scanning-indicator {
    font-size: 11px;
    color: var(--md-sys-color-primary);
    font-style: italic;
  }

  .directory-pill {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-small);
  }

  .dir-name {
    font-size: 13px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
  }

  .actions-container {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    gap: 12px;
  }

  .actions-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .error-banner {
    background: var(--md-sys-color-error-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    color: var(--md-sys-color-on-error-container);
    padding: 10px 14px;
    border-radius: var(--md-sys-shape-small);
    font-size: 13px;
    display: flex;
    align-items: center;
  }
</style>
