<script>
  import { Icon } from 'm3-svelte';
  let {
    title = '',
    icon = null,
    badge = null,
    badgeType = 'default',
    description = '',
    headerActions = null,
    children,
  } = $props();
</script>

<section class="md3-section-card">
  {#if title || icon || badge || headerActions}
    <div class="section-title-row">
      <div class="title-with-icon">
        {#if icon}
          <Icon icon={icon} size={20} class="title-icon" />
        {/if}
        {#if title}
          <h3 class="section-title">{title}</h3>
        {/if}
      </div>

      <div class="header-right">
        {#if badge}
          <span class="count-pill {badgeType}">{badge}</span>
        {/if}
        {#if headerActions}
          {@render headerActions()}
        {/if}
      </div>
    </div>
  {/if}

  {#if description}
    <p class="section-desc">{description}</p>
  {/if}

  <div class="section-content">
    {@render children?.()}
  </div>
</section>

<style>
  .md3-section-card {
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: var(--md-sys-shape-large);
    padding: 24px;
    margin-bottom: 20px;
    display: flex;
    flex-direction: column;
    box-shadow: var(--md-sys-elevation-1);
    transition: background 0.2s ease, border-color 0.2s ease;
  }

  .section-title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 6px;
    flex-wrap: wrap;
  }

  .title-with-icon {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  :global(.title-icon) {
    color: var(--md-sys-color-primary);
    fill: currentColor;
    flex-shrink: 0;
  }

  .section-title {
    margin: 0;
    font-size: 1.08rem;
    font-weight: 600;
    color: var(--md-sys-color-on-surface);
    letter-spacing: -0.01em;
  }

  .header-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .count-pill {
    font-size: 0.76rem;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: var(--md-sys-shape-full);
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .count-pill.primary {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
  }

  .count-pill.success {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
  }

  .section-desc {
    margin: 0 0 16px 0;
    font-size: 0.85rem;
    color: var(--md-sys-color-on-surface-variant);
    line-height: 1.5;
  }

  .section-content {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
</style>
