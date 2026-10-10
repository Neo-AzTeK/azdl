<script>
  let { activeTab = $bindable('library') } = $props();

  const tabs = [
    {
      id: 'library',
      label: 'Library',
      icon: `M21 6H3c-1.1 0-2 .9-2 2v8c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2zm-10 7H8v3H6v-3H3v-2h3V8h2v3h3v2zm4.5 2c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm4-3c-.83 0-1.5-.67-1.5-1.5S18.67 9 19.5 9s1.5.67 1.5 1.5-.67 1.5-1.5 1.5z`,
    },
    {
      id: 'catalog',
      label: 'Discover',
      icon: `M4 6H2v14c0 1.1.9 2 2 2h14v-2H4V6zm16-4H8c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2zm0 14H8V4h12v12zm-7-2h2v-3h3V9h-3V6h-2v3H9v2h3v3z`,
    },
    {
      id: 'settings',
      label: 'Settings',
      icon: `M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.488.488 0 0 0-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 0 0-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z`,
    },
  ];
</script>

<nav class="nav-rail">
  <div class="rail-items">
    {#each tabs as tab}
      <button
        class="rail-item"
        class:active={activeTab === tab.id}
        onclick={() => (activeTab = tab.id)}
        title={tab.label}
      >
        <div class="icon-container">
          <svg viewBox="0 0 24 24" width="22" height="22" fill="currentColor">
            <path d={tab.icon} />
          </svg>
        </div>
        <span class="label">{tab.label}</span>
      </button>
    {/each}
  </div>
</nav>

<style>
  .nav-rail {
    width: 76px;
    min-width: 76px;
    background: var(--md-sys-color-surface-container-low);
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 16px 0;
    border-right: 1px solid var(--md-sys-color-outline-variant);
    user-select: none;
    -webkit-user-select: none;
    z-index: 50;
  }

  .rail-items {
    display: flex;
    flex-direction: column;
    gap: 16px;
    width: 100%;
    align-items: center;
  }

  .rail-item {
    background: none;
    border: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    cursor: pointer;
    padding: 4px 0;
    width: 64px;
    color: var(--md-sys-color-on-surface-variant);
    transition: all 180ms cubic-bezier(0.2, 0, 0, 1);
  }

  .icon-container {
    width: 56px;
    height: 32px;
    border-radius: 16px;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 200ms cubic-bezier(0.2, 0, 0, 1);
  }

  .label {
    font-size: 11px;
    font-weight: 500;
    letter-spacing: 0.02em;
    color: inherit;
    transition: color 180ms ease;
  }

  .rail-item:hover .icon-container {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
  }

  .rail-item.active {
    color: var(--md-sys-color-on-surface);
  }

  .rail-item.active .icon-container {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .rail-item.active .label {
    font-weight: 700;
    color: var(--md-sys-color-on-surface);
  }
</style>
