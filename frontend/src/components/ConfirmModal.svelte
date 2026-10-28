<script>
  import { Icon } from 'm3-svelte';
  import iconError from '@ktibow/iconset-material-symbols/error';
  import iconInfoOutline from '@ktibow/iconset-material-symbols/info-outline';
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    title = 'Confirm Action',
    message = 'Are you sure you want to proceed?',
    confirmText = 'Confirm',
    cancelText = 'Cancel',
    altText = null,
    isDestructive = true,
    onConfirm = null,
    onCancel = null,
    onAlt = null,
  } = $props();

  let busy = $state(false);

  async function handleConfirm() {
    if (busy) return;
    busy = true;
    try {
      if (onConfirm) {
        await onConfirm();
      }
      open = false;
    } finally {
      busy = false;
    }
  }

  async function handleAlt() {
    if (busy) return;
    busy = true;
    try {
      if (onAlt) {
        await onAlt();
      }
      open = false;
    } finally {
      busy = false;
    }
  }

  function handleCancel() {
    if (busy) return;
    open = false;
    if (onCancel) onCancel();
  }
</script>

<Modal
  {open}
  title=""
  maxWidth="460px"
  onClose={handleCancel}
>
  <div class="confirm-modal-body" class:destructive={isDestructive}>
    {#if isDestructive}
      <div class="confirm-icon-wrap danger-icon">
        <Icon icon={iconError} size={28} />
      </div>
    {:else}
      <div class="confirm-icon-wrap info-icon">
        <Icon icon={iconInfoOutline} size={28} />
      </div>
    {/if}

    <div class="confirm-text">
      <h3 class="confirm-title" class:danger={isDestructive}>{title}</h3>
      <p class="confirm-message">{message}</p>
    </div>
  </div>

  {#snippet actions()}
    <div class="confirm-actions">
      <button
        type="button"
        class="md3-btn tonal"
        onclick={handleCancel}
        disabled={busy}
      >
        {cancelText}
      </button>

      {#if altText}
        <button
          type="button"
          class="md3-btn tonal"
          onclick={handleAlt}
          disabled={busy}
        >
          {altText}
        </button>
      {/if}

      <button
        type="button"
        class="md3-btn"
        class:danger={isDestructive}
        class:primary={!isDestructive}
        onclick={handleConfirm}
        disabled={busy}
      >
        {busy ? 'Processing...' : confirmText}
      </button>
    </div>
  {/snippet}
</Modal>

<style>
  .confirm-modal-body {
    display: flex;
    align-items: flex-start;
    gap: 16px;
    padding: 8px 0;
  }

  .confirm-icon-wrap {
    flex-shrink: 0;
    width: 44px;
    height: 44px;
    border-radius: var(--md-sys-shape-medium, 12px);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .confirm-icon-wrap.danger-icon {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  .confirm-icon-wrap.info-icon {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .confirm-text {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .confirm-title {
    font-size: 1.15rem;
    font-weight: 700;
    margin: 0;
    color: var(--md-sys-color-on-surface);
  }

  .confirm-title.danger {
    color: var(--md-sys-color-error);
  }

  .confirm-message {
    font-size: 0.9rem;
    line-height: 1.45;
    margin: 0;
    color: var(--md-sys-color-on-surface-variant);
  }

  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    width: 100%;
  }
</style>
