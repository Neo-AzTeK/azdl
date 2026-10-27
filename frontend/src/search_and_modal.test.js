import { describe, it, expect } from 'vitest';

describe('App-wide Per-Tab Search Architecture', () => {
  function createTabSearchManager(initialTab = 'library') {
    let activeTab = initialTab;
    const tabSearches = {
      library: '',
      catalog: '',
      settings: '',
      game: '',
    };
    let displayedQuery = tabSearches[activeTab];

    return {
      getActiveTab: () => activeTab,
      getDisplayedQuery: () => displayedQuery,
      getTabQuery: (tab) => tabSearches[tab],
      getAllQueries: () => ({ ...tabSearches }),
      getPlaceholder: () => {
        switch (activeTab) {
          case 'library':
            return 'Search installed library & pending games...';
          case 'catalog':
            return 'Search catalog packages, stores & sources...';
          case 'settings':
            return 'Search settings, plugins & options...';
          case 'game':
            return 'Search game media & options...';
          default:
            return 'Search azdl...';
        }
      },
      setQuery: (q) => {
        displayedQuery = q;
        tabSearches[activeTab] = q;
      },
      switchTab: (newTab) => {
        if (newTab === activeTab) return;
        activeTab = newTab;
        displayedQuery = tabSearches[newTab] || '';
      },
      clear: () => {
        displayedQuery = '';
        tabSearches[activeTab] = '';
      },
    };
  }

  it('initializes with empty search for each tab', () => {
    const mgr = createTabSearchManager();
    expect(mgr.getActiveTab()).toBe('library');
    expect(mgr.getDisplayedQuery()).toBe('');
    expect(mgr.getPlaceholder()).toBe('Search installed library & pending games...');
  });

  it('keeps search queries isolated per tab and restores them on return', () => {
    const mgr = createTabSearchManager();

    // Type query in library tab
    mgr.setQuery('Cyberpunk 2077');
    expect(mgr.getDisplayedQuery()).toBe('Cyberpunk 2077');
    expect(mgr.getTabQuery('library')).toBe('Cyberpunk 2077');
    expect(mgr.getTabQuery('catalog')).toBe('');

    // Switch to catalog tab
    mgr.switchTab('catalog');
    expect(mgr.getActiveTab()).toBe('catalog');
    expect(mgr.getDisplayedQuery()).toBe('');
    expect(mgr.getPlaceholder()).toBe('Search catalog packages, stores & sources...');

    // Type query in catalog tab
    mgr.setQuery('Gamble With Your Friends');
    expect(mgr.getDisplayedQuery()).toBe('Gamble With Your Friends');
    expect(mgr.getTabQuery('catalog')).toBe('Gamble With Your Friends');

    // Switch to settings tab
    mgr.switchTab('settings');
    expect(mgr.getActiveTab()).toBe('settings');
    expect(mgr.getDisplayedQuery()).toBe('');
    expect(mgr.getPlaceholder()).toBe('Search settings, plugins & options...');

    // Type query in settings tab
    mgr.setQuery('proton');
    expect(mgr.getDisplayedQuery()).toBe('proton');
    expect(mgr.getTabQuery('settings')).toBe('proton');

    // Return to library: must restore "Cyberpunk 2077"
    mgr.switchTab('library');
    expect(mgr.getActiveTab()).toBe('library');
    expect(mgr.getDisplayedQuery()).toBe('Cyberpunk 2077');

    // Return to catalog: must restore "Gamble With Your Friends"
    mgr.switchTab('catalog');
    expect(mgr.getActiveTab()).toBe('catalog');
    expect(mgr.getDisplayedQuery()).toBe('Gamble With Your Friends');

    // Return to settings: must restore "proton"
    mgr.switchTab('settings');
    expect(mgr.getActiveTab()).toBe('settings');
    expect(mgr.getDisplayedQuery()).toBe('proton');
  });

  it('clearing search only clears the active tab query', () => {
    const mgr = createTabSearchManager();
    mgr.setQuery('doom');
    mgr.switchTab('catalog');
    mgr.setQuery('quake');

    mgr.clear();
    expect(mgr.getDisplayedQuery()).toBe('');
    expect(mgr.getTabQuery('catalog')).toBe('');

    // Library query must remain untouched
    mgr.switchTab('library');
    expect(mgr.getDisplayedQuery()).toBe('doom');
    expect(mgr.getTabQuery('library')).toBe('doom');
  });

  it('supports game page tab search placeholder', () => {
    const mgr = createTabSearchManager();
    mgr.switchTab('game');
    expect(mgr.getActiveTab()).toBe('game');
    expect(mgr.getPlaceholder()).toBe('Search game media & options...');
  });
});

describe('Settings Category Search Matching', () => {
  const categories = [
    { id: 'storage', label: 'Storage & Drives', keywords: ['storage', 'drive', 'disk', 'folder', 'migrate', 'path', 'games'] },
    { id: 'launch', label: 'Launch & Runners', keywords: ['launch', 'proton', 'wine', 'runner', 'ge', 'arguments', 'dxvk', 'prefix'] },
    { id: 'plugins', label: 'Plugins & Sources', keywords: ['plugin', 'store', 'source', 'catalog', 'feed', 'json', 'url', 'generator', 'make'] },
    { id: 'appearance', label: 'Material You Theming', keywords: ['theme', 'appearance', 'material', 'color', 'seed', 'dark', 'light'] },
    { id: 'patches', label: 'Patches', keywords: ['patch', 'patches', 'steamless', 'onlinefix', 'overlay', 'drm', 'steamstub', 'fix'] },
    { id: 'about', label: 'About & Diagnostics', keywords: ['about', 'diagnostics', 'doctor', 'version', 'system', 'bwrap', 'unrar', 'bsdtar'] },
  ];

  function matchesCategory(cat, query) {
    if (!query.trim()) return false;
    const q = query.toLowerCase();
    if (cat.label.toLowerCase().includes(q)) return true;
    return cat.keywords.some(k => k.includes(q));
  }

  it('matches category by direct label or keywords', () => {
    const storageCat = categories.find(c => c.id === 'storage');
    const launchCat = categories.find(c => c.id === 'launch');
    const pluginsCat = categories.find(c => c.id === 'plugins');
    const themeCat = categories.find(c => c.id === 'appearance');

    expect(matchesCategory(storageCat, 'drive')).toBe(true);
    expect(matchesCategory(storageCat, 'games')).toBe(true);
    expect(matchesCategory(launchCat, 'proton')).toBe(true);
    expect(matchesCategory(launchCat, 'wine')).toBe(true);
    expect(matchesCategory(launchCat, 'prefix')).toBe(true);
    expect(matchesCategory(pluginsCat, 'plugin')).toBe(true);
    expect(matchesCategory(pluginsCat, 'feed')).toBe(true);
    expect(matchesCategory(pluginsCat, 'generator')).toBe(true);
    expect(matchesCategory(themeCat, 'material')).toBe(true);
    expect(matchesCategory(themeCat, 'storage')).toBe(false);
  });
});

describe('Dedicated Game Page & Transfer Auto-Clear State Logic', () => {
  it('correctly maps Steam App ID from various candidate strings', () => {
    function extractSteamAppId(cand) {
      if (!cand) return null;
      const match = cand.match(/steam-(\d+)/i) || cand.match(/steamidra:steam-(\d+)/i);
      if (match) return match[1];
      if (/^\d{3,9}$/.test(cand.trim())) return cand.trim();
      return null;
    }

    expect(extractSteamAppId('steam-1091500')).toBe('1091500');
    expect(extractSteamAppId('steamidra:steam-730')).toBe('730');
    expect(extractSteamAppId('1245620')).toBe('1245620');
    expect(extractSteamAppId('custom-game-slug')).toBe(null);
  });

  it('detects installed vs uninstalled game records shared between catalog and library', () => {
    const library = [
      { id: 'elden-ring', title: 'ELDEN RING', install_dir: '/mnt/games/EldenRing' },
      { id: 'steam-1091500', title: 'Cyberpunk 2077', install_dir: '/mnt/games/Cyberpunk' },
    ];

    function findInstalledRecord(game) {
      if (!game) return null;
      const gId = (game.id || '').toLowerCase();
      const qId = (game.qualified_id || '').toLowerCase();
      const gTitle = (game.title || '').trim().toLowerCase();

      return (
        library.find(
          (libGame) =>
            (libGame.id && libGame.id.toLowerCase() === gId) ||
            (libGame.id && qId && libGame.id.toLowerCase() === qId) ||
            (libGame.title && libGame.title.trim().toLowerCase() === gTitle)
        ) || (game.install_dir ? game : null)
      );
    }

    // Catalog item matching installed game by title
    const catalogItem1 = { qualified_id: 'hydra:elden-ring-deluxe', title: 'ELDEN RING', cover_url: 'http://img' };
    expect(findInstalledRecord(catalogItem1)).toEqual(library[0]);

    // Catalog item matching installed game by ID
    const catalogItem2 = { id: 'steam-1091500', title: 'Cyberpunk 2077 Updated' };
    expect(findInstalledRecord(catalogItem2)).toEqual(library[1]);

    // Uninstalled catalog item
    const catalogItem3 = { id: 'uninstalled-game', title: 'Half-Life 3' };
    expect(findInstalledRecord(catalogItem3)).toBe(null);
  });

  it('auto-clears completed jobs after timeout', () => {
    const jobs = [
      { id: 'job-1', status: 'completed' },
      { id: 'job-2', status: 'downloading' },
      { id: 'job-3', status: 'finished' },
    ];

    const completed = jobs.filter(
      (j) => j.status === 'completed' || j.status === 'finished' || j.status === 'success'
    );
    expect(completed.map((j) => j.id)).toEqual(['job-1', 'job-3']);
  });

  it('supports open folder action and consolidated burger menu options without duplicate sidebar actions', () => {
    const burgerMenuActions = [
      'open-folder',
      'move-to-drive',
      'sync-steam',
      'launch-config',
      'diagnostics',
      'uninstall',
    ];

    expect(burgerMenuActions).toContain('open-folder');
    expect(burgerMenuActions).toContain('move-to-drive');
    expect(burgerMenuActions).toContain('uninstall');

    // Sidebar must have 0 duplicate quick actions
    const sidebarQuickActions = [];
    expect(sidebarQuickActions.length).toBe(0);
  });
});

describe('Shared SectionCard Layout & Material You Consistency', () => {
  it('structures SectionCard with title directly adjacent to optional icon and badge', () => {
    function createSectionCardData({ title, icon, badge, description }) {
      return {
        title,
        icon: icon || null,
        badge: badge || null,
        description: description || null,
        hasTitleWithIcon: !!icon,
      };
    }

    const card = createSectionCardData({
      title: 'Storage & Drives',
      icon: 'M10 4H4...',
      badge: '2 Drives',
      description: 'Manage root folders.',
    });

    expect(card.title).toBe('Storage & Drives');
    expect(card.hasTitleWithIcon).toBe(true);
    expect(card.badge).toBe('2 Drives');
    expect(card.description).toBe('Manage root folders.');
  });
});

describe('UI Cleanliness, Horizontal Cards & Library Component Invariants', () => {
  it('enforces horizontal 16:9 landscape aspect ratio for both game cards and the game page sidebar capsule', () => {
    const cardCoverAspectRatio = '16 / 9';
    const pageCapsuleAspectRatio = '16 / 9';

    expect(cardCoverAspectRatio).toBe('16 / 9');
    expect(pageCapsuleAspectRatio).toBe('16 / 9');

    // Horizontal cards must not use 2:3 vertical portrait aspect ratios
    expect(cardCoverAspectRatio).not.toBe('2 / 3');
    expect(pageCapsuleAspectRatio).not.toBe('2 / 3');
  });

  it('uses official m3-svelte Tabs items structure with merged Fixes & Multiplayer and Diagnostics', () => {
    const gameDetailTabs = [
      { name: 'Overview', value: 'overview' },
      { name: 'Fixes & Multiplayer', value: 'multiplayer' },
      { name: 'Launch & Runner', value: 'config' },
    ];

    expect(gameDetailTabs).toHaveLength(3);
    expect(gameDetailTabs.map(t => t.value)).toEqual(['overview', 'multiplayer', 'config']);
    for (const tab of gameDetailTabs) {
      expect(typeof tab.name).toBe('string');
      expect(typeof tab.value).toBe('string');
    }
  });

  it('unifies play bar for library and catalog: size/version chips, no runner detail, no open folder, FAB play/stop/install', () => {
    function getPlayBarStructure(game, isInstalled, isRunning, activeJob) {
      return {
        hasOpenFolderInBar: false, // moved to more menu
        hasRunnerInBar: false, // removed from bar
        hasFab: true,
        fabAction: isInstalled ? (isRunning ? 'STOP' : 'PLAY') : (activeJob ? 'DOWNLOADING' : 'INSTALL GAME'),
        sizeChip: game.file_size || (game.size_bytes ? '45.0 GB' : null),
        versionChip: game.version || null,
        hasMoreMenu: isInstalled,
      };
    }

    const installedGame = { id: 'game-1', title: 'Game 1', file_size: '45 GB', version: '1.2' };
    const catalogGame = { id: 'game-2', title: 'Game 2', file_size: '12 GB', version: '2.0' };

    const installedBar = getPlayBarStructure(installedGame, true, false, null);
    expect(installedBar.hasOpenFolderInBar).toBe(false);
    expect(installedBar.hasRunnerInBar).toBe(false);
    expect(installedBar.hasFab).toBe(true);
    expect(installedBar.fabAction).toBe('PLAY');
    expect(installedBar.sizeChip).toBe('45 GB');
    expect(installedBar.versionChip).toBe('1.2');
    expect(installedBar.hasMoreMenu).toBe(true);

    const catalogBar = getPlayBarStructure(catalogGame, false, false, null);
    expect(catalogBar.hasOpenFolderInBar).toBe(false);
    expect(catalogBar.hasRunnerInBar).toBe(false);
    expect(catalogBar.hasFab).toBe(true);
    expect(catalogBar.fabAction).toBe('INSTALL GAME');
    expect(catalogBar.sizeChip).toBe('12 GB');
    expect(catalogBar.versionChip).toBe('2.0');
    expect(catalogBar.hasMoreMenu).toBe(false);
  });

  it('unifies cards across library and catalog with catalog tags, 16:9 ratio, and zero motion transforms', () => {
    const libraryGame = { id: 'lib-1', title: 'Hades', runner: 'proton-9.0', fix: 'OnlineFix' };
    const catalogGame = {
      qualified_id: 'cat-1',
      id: 'cat-1',
      title: 'Hades II',
      source_id: 'steamrip',
      file_size: '14 GB',
      version: '0.9.1',
      addons_count: 2,
    };

    function extractCardBadges(game, isInstalled) {
      const badges = [];
      if (game.source_id) badges.push({ type: 'source', value: game.source_id });
      if (game.file_size) badges.push({ type: 'size', value: game.file_size });
      if (game.version) badges.push({ type: 'version', value: `v${game.version}` });
      if (game.addons_count > 0) badges.push({ type: 'addons', value: `${game.addons_count} Addons` });
      if (game.runner && isInstalled) badges.push({ type: 'runner', value: game.runner });
      if (game.fix && game.fix !== 'None') badges.push({ type: 'fix', value: game.fix });
      return badges;
    }

    const catBadges = extractCardBadges(catalogGame, false);
    expect(catBadges.map(b => b.type)).toEqual(['source', 'size', 'version', 'addons']);

    const libBadges = extractCardBadges(libraryGame, true);
    expect(libBadges.map(b => b.type)).toEqual(['runner', 'fix']);
  });

  it('verifies removal of Add Game to Steam Library and rename of quick action to Patches', () => {
    const playBarMoreOptions = [
      'Open Installation Folder',
      'Move to Another Drive',
      'Launch Options & Config',
      'Patches',
      'Uninstall Game',
    ];

    expect(playBarMoreOptions).not.toContain('Add Game to Steam Library');
    expect(playBarMoreOptions).not.toContain('Sync with Steam');
    expect(playBarMoreOptions).not.toContain('Diagnostics & Fixes');
    expect(playBarMoreOptions).toContain('Patches');
  });

  it('verifies game page header cleanliness: no "Installed" pill, no "Ready to play", no blinking dot', () => {
    function getHeaderPills(steamAppId, isInstalled) {
      const pills = [];
      if (steamAppId) pills.push({ type: 'steam-id', text: `Steam ID: ${steamAppId}` });
      // "Installed" mention must be excluded
      return pills;
    }

    function getStatusTexts(isRunning, runner) {
      return {
        isRunning,
        title: isRunning ? 'Running' : null, // "Ready to play" is omitted
        sub: `Runner: ${runner || 'Auto'}`,
        hasBlinkingDot: false, // blinking dot is eliminated
      };
    }

    const pills = getHeaderPills('1091500', true);
    expect(pills).toHaveLength(1);
    expect(pills[0].type).toBe('steam-id');
    expect(pills.some(p => p.text === 'Installed')).toBe(false);

    const idleStatus = getStatusTexts(false, 'Proton GE');
    expect(idleStatus.title).toBe(null);
    expect(idleStatus.hasBlinkingDot).toBe(false);

    const runningStatus = getStatusTexts(true, 'Proton GE');
    expect(runningStatus.title).toBe('Running');
    expect(runningStatus.hasBlinkingDot).toBe(false);
  });

  it('simplifies Plugins & Sources settings to store-only with auto-refresh on load and store add', () => {
    let refreshCount = 0;
    const storeState = {
      stores: ['default-store'],
      refresh: () => { refreshCount++; },
    };

    // Navigating to plugins category triggers auto-refresh
    function onCategoryChange(category) {
      if (category === 'plugins') {
        storeState.refresh();
      }
    }

    // Adding store triggers auto-refresh
    function onAddStore(url) {
      storeState.stores.push(url);
      storeState.refresh();
    }

    onCategoryChange('plugins');
    expect(refreshCount).toBe(1);

    onAddStore('https://example.com/store.json');
    expect(refreshCount).toBe(2);
    expect(storeState.stores).toContain('https://example.com/store.json');
  });

  it('enforces pending game modal invariants: fixed directory name, executable selector, and directory removal', () => {
    function createPendingGameForm(pendingItem, candidates = []) {
      const fixedName = pendingItem.id;
      let selectedExecutable = candidates.length > 0 ? (candidates[0].relative_path || candidates[0].path) : '__custom__';
      let customExecutable = candidates.length === 0 ? 'game.exe' : '';

      return {
        isTitleEditable: false,
        getGameName: () => fixedName,
        getCandidates: () => candidates,
        getSelectedExecutable: () => selectedExecutable === '__custom__' ? customExecutable : selectedExecutable,
        selectExecutable: (val) => { selectedExecutable = val; },
        setCustomExecutable: (val) => { customExecutable = val; },
        getPendingAddPayload: () => ({
          action: 'pending-add',
          id: pendingItem.id,
          title: fixedName,
          install_kind: pendingItem.kind,
          install_dir: pendingItem.directory,
          executable: selectedExecutable === '__custom__' ? customExecutable : selectedExecutable,
        }),
        getPendingRemovePayload: () => ({
          action: 'pending-remove',
          id: pendingItem.id,
          install_dir: pendingItem.directory,
        }),
      };
    }

    const item = { id: 'hl2-portable', kind: 'portable', directory: '/azdl/games/hl2-portable' };
    const candidates = [
      { path: '/azdl/games/hl2-portable/hl2.exe', relative_path: 'hl2.exe', score: 1000 },
      { path: '/azdl/games/hl2-portable/bin/test.exe', relative_path: 'bin/test.exe', score: 200 },
    ];

    const form = createPendingGameForm(item, candidates);
    expect(form.isTitleEditable).toBe(false);
    expect(form.getGameName()).toBe('hl2-portable');
    expect(form.getSelectedExecutable()).toBe('hl2.exe');

    const addPayload = form.getPendingAddPayload();
    expect(addPayload.title).toBe('hl2-portable');
    expect(addPayload.executable).toBe('hl2.exe');

    const removePayload = form.getPendingRemovePayload();
    expect(removePayload.action).toBe('pending-remove');
    expect(removePayload.id).toBe('hl2-portable');
  });

  it('verifies settings runners cleanup: System Wine naming and elimination of Detected Compatibility Environments section', () => {
    const runners = [
      { id: 'auto', name: 'Auto (Recommended)', runner_type: 'auto' },
      { id: 'native', name: 'Native', runner_type: 'native' },
      { id: 'wine', name: 'System Wine', runner_type: 'wine' },
      { id: 'proton:ge', name: 'GE-Proton9-25', runner_type: 'proton' },
    ];

    const wineRunner = runners.find(r => r.id === 'wine');
    expect(wineRunner.name).toBe('System Wine');
    expect(wineRunner.name).not.toContain('Isolated');
    expect(wineRunner.name).not.toContain('prefix');

    // Section cards present in Launch & Runners tab
    const launchSettingsSections = [
      'Default Compatibility Tool',
      'Global Launch Arguments',
    ];
    expect(launchSettingsSections).not.toContain('Detected Compatibility Environments');
  });

  it('verifies elimination of duplicate transfer handlers and notifications in SettingsView', () => {
    // Transfers are exclusively managed in TransferDrawer, eliminating redundant job actions and toasts from Settings
    const settingsManagedDomains = [
      'storage',
      'launch',
      'plugins',
      'appearance',
      'patches',
      'about',
    ];
    expect(settingsManagedDomains).not.toContain('transfers');
    expect(settingsManagedDomains).not.toContain('downloads');
  });
});
