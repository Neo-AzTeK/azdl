<script>
  let {
    open = false,
    title = '',
    subtitle = '',
    maxWidth = '520px',
    onClose = null,
    children,
    actions,
  } = $props();

  function handleBackdrop(e) {
    if (e.target === e.currentTarget && onClose) {
      onClose();
    }
  }

  function handleKeydown(e) {
    if (e.key === 'Escape' && onClose) {
      onClose();
    }
  }
</script>

{#if open}
  <div
    class="md3-modal-backdrop"
    onclick={handleBackdrop}
    onkeydown={handleKeydown}
    role="presentation"
    tabindex="-1"
  >
    <div
      class="md3-modal-surface"
      style="max-width: {maxWidth};"
      role="dialog"
      aria-modal="true"
      aria-labelledby="modal-title"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
    >
      <header class="modal-header">
        <div class="header-text">
          {#if title}
            <h3 id="modal-title" class="modal-title">{title}</h3>
          {/if}
          {#if subtitle}
            <p class="modal-subtitle">{subtitle}</p>
          {/if}
        </div>
        {#if onClose}
          <button
            class="modal-close-btn"
            onclick={onClose}
            aria-label="Close dialog"
            title="Close"
          >
            <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor">
              <path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/>
            </svg>
          </button>
        {/if}
      </header>

      <div class="modal-body">
        {#if children}
          {@render children()}
        {/if}
      </div>

      {#if actions}
        <footer class="modal-actions">
          {@render actions()}
        </footer>
      {/if}
    </div>
  </div>
{/if}

<style>
  .md3-modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.65);
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 20px;
    animation: fadeIn 0.18s cubic-bezier(0, 0, 0.2, 1);
  }

  .md3-modal-surface {
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-extra-large, 24px);
    box-shadow: 0 16px 40px rgba(0, 0, 0, 0.45);
    width: 100%;
    max-height: 90vh;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    animation: scaleUp 0.2s cubic-bezier(0, 0, 0.2, 1);
  }

  .modal-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    padding: 24px 24px 16px 24px;
    gap: 16px;
    border-bottom: 1px solid var(--md-sys-color-outline-variant);
  }

  .header-text {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
  }

  .modal-title {
    margin: 0;
    font-size: 1.25rem;
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
    letter-spacing: -0.01em;
  }

  .modal-subtitle {
    margin: 0;
    font-size: 0.85rem;
    color: var(--md-sys-color-on-surface-variant);
    line-height: 1.4;
  }

  .modal-close-btn {
    background: transparent;
    border: none;
    color: var(--md-sys-color-on-surface-variant);
    width: 36px;
    height: 36px;
    border-radius: var(--md-sys-shape-full, 50%);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: background 0.15s ease, color 0.15s ease;
    flex-shrink: 0;
  }

  .modal-close-btn:hover {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
  }

  .modal-body {
    padding: 24px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .modal-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 12px;
    padding: 16px 24px 24px 24px;
    border-top: 1px solid var(--md-sys-color-outline-variant);
    background: var(--md-sys-color-surface-container-high);
  }

  @keyframes fadeIn {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }

  @keyframes scaleUp {
    from {
      opacity: 0;
      transform: scale(0.95);
    }
    to {
      opacity: 1;
      transform: scale(1);
    }
  }
</style>
