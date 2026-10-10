<script>
  import Modal from './Modal.svelte';

  let {
    open = $bindable(false),
    title = 'Confirm Action',
    message = 'Are you sure you want to proceed?',
    confirmText = 'Confirm',
    cancelText = 'Cancel',
    isDestructive = true,
    onConfirm = null,
    onCancel = null,
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
        <svg viewBox="0 0 24 24" width="28" height="28" fill="currentColor">
          <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2zm0-4h-2V7h2v6z"/>
        </svg>
      </div>
    {:else}
      <div class="confirm-icon-wrap info-icon">
        <svg viewBox="0 0 24 24" width="28" height="28" fill="currentColor">
          <path d="M11 17h2v-6h-2v6zm1-15C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 18c-4.41 0-8-3.59-8-8s3.59-8 8-8 8 3.59 8 8-3.59 8-8 8zM11 9h2V7h-2v2z"/>
        </svg>
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
