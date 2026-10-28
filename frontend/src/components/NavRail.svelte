<script>
  import { Icon } from 'm3-svelte';
  import iconLibraryAdd from '@ktibow/iconset-material-symbols/library-add';
  import iconSettings from '@ktibow/iconset-material-symbols/settings';
  import iconVideogameAsset from '@ktibow/iconset-material-symbols/videogame-asset';
  let { activeTab = $bindable('library') } = $props();

  const tabs = [
    {
      id: 'library',
      label: 'Library',
      icon: {iconVideogameAsset},
    },
    {
      id: 'catalog',
      label: 'Discover',
      icon: {iconLibraryAdd},
    },
    {
      id: 'settings',
      label: 'Settings',
      icon: {iconSettings},
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
          <Icon icon={tab.icon} size={22} />
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
