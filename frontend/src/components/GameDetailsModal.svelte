<script>
  import { onMount } from 'svelte';
  import { Tabs } from 'm3-svelte';
  import {
    launchGame,
    stopGame,
    runAction,
    diagnoseGame,
    discoverExecutables,
    scanSetup,
  } from '../api.js';
  import { extractCoverAccent } from '../theme.js';

  let {
    game,
    runners = [],
    onClose,
    onRefresh,
  } = $props();

  let activeTab = $state('health'); // 'health' | 'multiplayer' | 'patch' | 'runner'

  // Diagnostic state
  let loadingDiag = $state(true);
  let diagReport = $state(null);

  // Executables scanner state
  let discoveringExes = $state(false);
  let candidates = $state([]);

  // Setup prefix scanner state
  let scanningPrefix = $state(false);
  let setupCandidates = $state([]);

  // Dynamic extracted accent color
  let coverAccent = $state(null);

  // Form states for runner & options
  let selectedRunner = $state('');
  let selectedExe = $state('');
  let argumentsInput = $state('');

  $effect(() => {
    selectedRunner = game.runner;
    selectedExe = game.executable;
    argumentsInput = (game.arguments || []).join(' ');
  });

  let envKey = $state('');
  let envVal = $state('');

  // Status message for feedback
  let actionStatus = $state(null);

  onMount(async () => {
    // 1. Extract cover accent and tint the dialog
    if (game.cover_url) {
      try {
        const accent = await extractCoverAccent(game.cover_url);
        if (accent) {
          coverAccent = accent;
        }
      } catch (e) {
        console.error('Failed to extract cover accent:', e);
      }
    }

    // 2. Fetch diagnostics
    await refreshDiagnostics();
  });

  async function refreshDiagnostics() {
    loadingDiag = true;
    try {
      diagReport = await diagnoseGame(game.id);
    } catch (e) {
      console.error('Diagnostic failed:', e);
    } finally {
      loadingDiag = false;
    }
  }

  async function handleAction(actionPayload, successMsg) {
    actionStatus = { type: 'working', msg: 'Executing...' };
    try {
      await runAction({ id: game.id, ...actionPayload });
      actionStatus = { type: 'success', msg: successMsg };
      if (onRefresh) onRefresh();
      await refreshDiagnostics();
      setTimeout(() => (actionStatus = null), 4000);
    } catch (err) {
      actionStatus = { type: 'error', msg: String(err) };
    }
  }

  async function handleSetRunner() {
    await handleAction(
      { action: 'runner-set', runner: selectedRunner },
      `Runner set to ${selectedRunner}`
    );
  }

  async function handleSetExecutable(exePath) {
    selectedExe = exePath;
    await handleAction(
      { action: 'executable-set', executable: exePath },
      `Executable set to ${exePath}`
    );
  }

  async function handleSetArguments() {
    const args = argumentsInput
      .split(' ')
      .map((s) => s.trim())
      .filter(Boolean);
    await handleAction(
      { action: 'args-set', arguments: args },
      'Launch arguments updated'
    );
  }

  async function handleAddEnv() {
    if (!envKey.trim()) return;
    await handleAction(
      { action: 'env-set', key: envKey.trim(), value: envVal.trim() },
      `Set environment variable ${envKey}`
    );
    envKey = '';
    envVal = '';
  }

  async function handleRemoveEnv(key) {
    await handleAction(
      { action: 'env-remove', key },
      `Removed environment variable ${key}`
    );
  }

  async function scanCandidates() {
    discoveringExes = true;
    try {
      candidates = await discoverExecutables(game.id);
    } catch (e) {
      console.error(e);
    } finally {
      discoveringExes = false;
    }
  }

  async function scanSetupPrefix() {
    scanningPrefix = true;
    try {
      setupCandidates = await scanSetup(game.id, game.title);
    } catch (e) {
      console.error(e);
    } finally {
      scanningPrefix = false;
    }
  }

  function handleCloseModal() {
    onClose();
  }
</script>

<div
  class="modal-backdrop"
  onclick={handleCloseModal}
  onkeydown={(e) => e.key === 'Escape' && handleCloseModal()}
  role="presentation"
>
  <div
    class="modal-surface"
    style:--cover-tint={coverAccent || 'var(--md-sys-color-primary)'}
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
    role="dialog"
    aria-modal="true"
    tabindex="-1"
  >
    <!-- Header with Cover Accent Tinting -->
    <header class="modal-header">
      <div class="header-banner">
        {#if game.banner_url || game.cover_url}
          <img src={game.banner_url || game.cover_url} alt="" class="header-bg-img" decoding="async" />
        {/if}
        <div class="header-scrim"></div>
        <div class="header-info">
          <h2 class="game-title">{game.title}</h2>
          <span class="game-path">{game.install_dir}</span>
        </div>
        <button class="close-icon-btn" onclick={handleCloseModal} title="Close">✕</button>
      </div>

      <!-- Navigation Tabs -->
      <div class="dialog-tabs-wrapper">
        <Tabs
          bind:tab={activeTab}
          items={[
            { name: 'Health & Doctor', value: 'health' },
            { name: 'Fixes & Multiplayer', value: 'multiplayer' },
            { name: 'Patches & Rollback', value: 'patch' },
            { name: 'Runner & Config', value: 'runner' },
          ]}
        />
      </div>
    </header>

    <!-- Feedback Notice Banner -->
    {#if actionStatus}
      <div class="status-banner status-{actionStatus.type}">
        <span>{actionStatus.msg}</span>
      </div>
    {/if}

    <!-- Tab Content -->
    <div class="modal-body">
      <!-- TAB 1: HEALTH & DOCTOR -->
      {#if activeTab === 'health'}
        <div class="tab-pane">
          <div class="section-title-row">
            <h3>Environment Diagnostics</h3>
            <button class="small-action-btn" onclick={refreshDiagnostics} disabled={loadingDiag}>
              {loadingDiag ? 'Scanning...' : 'Re-scan'}
            </button>
          </div>

          {#if loadingDiag}
            <div class="loading-box">Analyzing game executables, runners, and prefix...</div>
          {:else if diagReport}
            <div class="diag-summary">
              <span class="diag-runner">Resolved Runner: <strong>{diagReport.runner || 'None'}</strong></span>
            </div>

            <div class="checks-list">
              {#if diagReport.checks.length === 0}
                <div class="check-item check-info">All checks passed. System ready for launch.</div>
              {:else}
                {#each diagReport.checks as check}
                  <div class="check-item check-{check.severity}">
                    <span class="severity-tag">{check.severity}</span>
                    <span class="check-msg">{check.message}</span>
                  </div>
                {/each}
              {/if}
            </div>
          {/if}
        </div>

      <!-- TAB 2: MULTIPLAYER & FIXES -->
      {:else if activeTab === 'multiplayer'}
        <div class="tab-pane">
          <h3>Multiplayer & Runtime Fix Overlays</h3>
          <p class="section-desc">
            Reversible SteamFix, Online-Fix, and SteamStub protection handling.
          </p>

          <div class="fix-cards-grid">
            <!-- Overlay (.arachnel-off) Toggle -->
            <div class="action-card">
              <div class="action-card-header">
                <h4>DLL Overlay Status</h4>
                <span class="card-status-chip">
                  {game.fix && game.fix !== 'None' ? game.fix : 'Clean'}
                </span>
              </div>
              <p class="card-desc">
                Toggle all multiplayer fix DLLs and FakeAppId via reversible <code>.arachnel-off</code> renames.
              </p>
              <div class="card-footer">
                <button
                  class="card-btn primary"
                  onclick={() => handleAction({ action: 'overlay-toggle' }, 'Toggled DLL overlay')}
                >
                  Toggle DLL Overlay
                </button>
              </div>
            </div>

            <!-- SteamStub Unpacker (Steamless) -->
            <div class="action-card">
              <div class="action-card-header">
                <h4>SteamStub DRM Unpacker</h4>
                <span class="card-status-chip">
                  {game.steamstub?.detected ? 'Detected' : 'Not Detected'}
                </span>
              </div>
              <p class="card-desc">
                Strip SteamStub PE DRM (.bind section) using Steamless with automated <code>.steamstub.bak</code> backups.
              </p>
              <div class="card-footer">
                <button
                  class="card-btn"
                  onclick={() => handleAction({ action: 'steamstub-unpack' }, 'SteamStub unpacked')}
                >
                  Unpack SteamStub
                </button>
                <button
                  class="card-btn secondary"
                  onclick={() => handleAction({ action: 'steamstub-rollback' }, 'SteamStub restored')}
                >
                  Rollback
                </button>
              </div>
            </div>

            <!-- EA Frostbite Activation Jump Fix -->
            <div class="action-card">
              <div class="action-card-header">
                <h4>EA Activation Patcher</h4>
                <span class="card-status-chip">
                  {game.ea_heal_applied ? 'Applied' : game.ea_heal_available ? 'Available' : 'N/A'}
                </span>
              </div>
              <p class="card-desc">
                Flip Frostbite bytecode jumps in Activation.dll / Activation64.dll with <code>.ea.bak</code> rollback.
              </p>
              <div class="card-footer">
                <button
                  class="card-btn"
                  disabled={!game.ea_heal_available && !game.ea_heal_applied}
                  onclick={() => handleAction({ action: 'ea-heal' }, 'EA activation healed')}
                >
                  Apply EA Jump Flip
                </button>
                <button
                  class="card-btn secondary"
                  disabled={!game.ea_heal_applied}
                  onclick={() => handleAction({ action: 'ea-rollback' }, 'EA activation rolled back')}
                >
                  Rollback EA
                </button>
              </div>
            </div>
          </div>
        </div>

      <!-- TAB 3: PATCHES & ROLLBACK -->
      {:else if activeTab === 'patch'}
        <div class="tab-pane">
          <h3>Source Patch Management</h3>
          <p class="section-desc">
            Reversible overlay patches with atomic staging manifests and clean rollbacks.
          </p>

          <div class="patch-box">
            <div class="patch-info-row">
              <span class="patch-label">Active Patch:</span>
              <span class="patch-val">
                {game.patch ? `Installed (${game.patch})` : 'No active patch'}
              </span>
            </div>

            <div class="patch-actions-row">
              {#if game.patch}
                <button
                  class="card-btn"
                  onclick={() => handleAction({ action: 'patch-toggle' }, 'Patch status toggled')}
                >
                  {game.patch === 'enabled' ? 'Disable Patch' : 'Enable Patch'}
                </button>
                <button
                  class="card-btn danger"
                  onclick={() => handleAction({ action: 'patch-rollback' }, 'Patch rolled back')}
                >
                  Rollback Patch
                </button>
              {:else}
                <p class="hint-text">
                  To apply patches, install an addon package from the Catalog or run a local patch archive.
                </p>
              {/if}
            </div>
          </div>
        </div>

      <!-- TAB 4: RUNNER & CONFIG -->
      {:else if activeTab === 'runner'}
        <div class="tab-pane">
          <h3>Runtime & Launch Configuration</h3>

          <!-- Runner Selection -->
          <div class="form-group">
            <label for="runner-select">Compatibility Runner</label>
            <div class="row-input">
              <select id="runner-select" bind:value={selectedRunner}>
                {#each runners as r}
                  <option value={r.id}>{r.name}</option>
                {/each}
              </select>
              <button class="save-btn" onclick={handleSetRunner}>Save Runner</button>
            </div>
          </div>

          <!-- Executable Selection -->
          <div class="form-group">
            <label for="exe-input">Main Executable</label>
            <div class="row-input">
              <input id="exe-input" type="text" bind:value={selectedExe} />
              <button class="save-btn" onclick={() => handleSetExecutable(selectedExe)}>Save</button>
              <button class="secondary-btn" onclick={scanCandidates} disabled={discoveringExes}>
                {discoveringExes ? 'Scanning...' : 'Auto-Discover'}
              </button>
            </div>
          </div>

          <!-- Discovered Candidate Executables -->
          {#if candidates.length > 0}
            <div class="candidates-panel">
              <h4>Candidate Executables (Heuristic Score):</h4>
              <div class="candidates-list">
                {#each candidates as cand}
                  <button
                    class="candidate-item"
                    onclick={() => handleSetExecutable(cand.path)}
                  >
                    <span class="cand-name">{cand.relative_path}</span>
                    <span class="cand-score">Score: {cand.score}</span>
                  </button>
                {/each}
              </div>
            </div>
          {/if}

          <!-- Launch Arguments -->
          <div class="form-group">
            <label for="args-input">Custom Launch Arguments</label>
            <div class="row-input">
              <input
                id="args-input"
                type="text"
                placeholder="e.g. -dx11 -novid +fps_max 144"
                bind:value={argumentsInput}
              />
              <button class="save-btn" onclick={handleSetArguments}>Save Args</button>
            </div>
          </div>

          <!-- Environment Variables -->
          <div class="form-group">
            <span class="field-label">Environment Overrides</span>
            <div class="env-table">
              {#if Object.keys(game.environment || {}).length === 0}
                <div class="empty-env">No custom environment variables set</div>
              {:else}
                {#each Object.entries(game.environment) as [k, v]}
                  <div class="env-row">
                    <span class="env-k"><code>{k}</code></span>
                    <span class="env-v"><code>{v}</code></span>
                    <button class="remove-env-btn" onclick={() => handleRemoveEnv(k)}>✕</button>
                  </div>
                {/each}
              {/if}
            </div>

            <div class="add-env-row">
              <input type="text" placeholder="Variable (e.g. PROTON_ENABLE_NVAPI)" bind:value={envKey} />
              <input type="text" placeholder="Value (e.g. 1)" bind:value={envVal} />
              <button class="save-btn" onclick={handleAddEnv}>Add Env</button>
            </div>
          </div>

          <!-- Shortcuts -->
          <div class="shortcuts-section">
            <h4>System Shortcuts Integration</h4>
            <div class="shortcut-actions">
              <button
                class="shortcut-btn"
                onclick={() => handleAction({ action: 'shortcut-create' }, 'Created desktop shortcut')}
              >
                Create Desktop Shortcut
              </button>
              <button
                class="shortcut-btn"
                onclick={() => handleAction({ action: 'shortcut-remove' }, 'Removed desktop shortcut')}
              >
                Remove Desktop Shortcut
              </button>
            </div>
          </div>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    backdrop-filter: blur(8px);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 200;
  }

  .modal-surface {
    width: 90%;
    max-width: 820px;
    max-height: 85vh;
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: 20px;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    box-shadow: 0 16px 40px rgba(0, 0, 0, 0.6);
  }

  .modal-header {
    position: relative;
    background: var(--md-sys-color-surface-container-high);
    border-bottom: 1px solid var(--md-sys-color-outline-variant);
  }

  .header-banner {
    position: relative;
    height: 120px;
    display: flex;
    align-items: flex-end;
    padding: 16px 20px;
    overflow: hidden;
  }

  .header-bg-img {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    filter: blur(14px) brightness(0.6);
    transform: scale(1.1);
  }

  .header-scrim {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: linear-gradient(
      to top,
      rgba(var(--md-sys-color-primary-rgb), 0.25),
      rgba(0, 0, 0, 0.6)
    );
  }

  .header-info {
    position: relative;
    z-index: 5;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .game-title {
    font-size: 20px;
    font-weight: 700;
    color: white;
  }

  .game-path {
    font-size: 11px;
    color: rgba(255, 255, 255, 0.7);
    font-family: monospace;
  }

  .close-icon-btn {
    position: absolute;
    top: 12px;
    right: 12px;
    z-index: 10;
    background: rgba(0, 0, 0, 0.5);
    border: none;
    color: white;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .dialog-tabs-wrapper {
    width: 100%;
    overflow: hidden;
  }

  .status-banner {
    padding: 8px 16px;
    font-size: 12px;
    font-weight: 500;
  }

  .status-banner.status-success {
    background: var(--md-sys-color-primary-container);
    color: var(--md-sys-color-on-primary-container);
  }

  .status-banner.status-error {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
  }

  .status-banner.status-working {
    background: var(--md-sys-color-secondary-container);
    color: var(--md-sys-color-on-secondary-container);
  }

  .modal-body {
    padding: 20px;
    overflow-y: auto;
    max-height: calc(85vh - 170px);
  }

  .tab-pane {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .section-title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .section-desc {
    font-size: 12px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .checks-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 8px;
  }

  .check-item {
    padding: 8px 12px;
    border-radius: var(--md-sys-shape-small);
    font-size: 12px;
    display: flex;
    align-items: center;
    gap: 10px;
    background: var(--md-sys-color-surface-container-high);
  }

  .check-item.check-error {
    background: var(--md-sys-color-error-container);
    color: var(--md-sys-color-on-error-container);
    border-left: 3px solid var(--md-sys-color-error);
  }

  .check-item.check-warning {
    background: var(--md-sys-color-tertiary-container);
    color: var(--md-sys-color-on-tertiary-container);
    border-left: 3px solid var(--md-sys-color-tertiary);
  }

  .check-item.check-info {
    border-left: 3px solid var(--md-sys-color-secondary);
  }

  .severity-tag {
    text-transform: uppercase;
    font-size: 10px;
    font-weight: 700;
  }

  .fix-cards-grid {
    display: grid;
    grid-template-columns: 1fr;
    gap: 12px;
  }

  .action-card {
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: 12px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .action-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .card-status-chip {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 12px;
    background: var(--md-sys-color-surface-container-highest);
  }

  .card-desc {
    font-size: 12px;
    color: var(--md-sys-color-on-surface-variant);
  }

  .card-footer {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }

  .card-btn {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
    border: none;
    border-radius: 8px;
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
  }

  .card-btn.primary {
    background: var(--cover-tint);
    color: white;
  }

  .card-btn.danger {
    background: var(--md-sys-color-error);
    color: var(--md-sys-color-on-error);
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .form-group label,
  .form-group .field-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--md-sys-color-on-surface-variant);
  }

  .row-input {
    display: flex;
    gap: 8px;
  }

  .row-input select,
  .row-input input {
    flex: 1;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    color: var(--md-sys-color-on-surface);
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 13px;
    font-family: inherit;
  }

  .row-input select {
    padding-right: 32px;
  }

  .save-btn,
  .secondary-btn,
  .small-action-btn {
    background: var(--md-sys-color-primary);
    color: var(--md-sys-color-on-primary);
    border: none;
    border-radius: 8px;
    padding: 6px 14px;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
  }

  .secondary-btn,
  .small-action-btn {
    background: var(--md-sys-color-surface-container-highest);
    color: var(--md-sys-color-on-surface);
  }

  .candidates-panel {
    background: var(--md-sys-color-surface-container-low);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: 10px;
    padding: 10px;
  }

  .candidates-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 6px;
  }

  .candidate-item {
    background: var(--md-sys-color-surface-container);
    border: 1px solid var(--md-sys-color-outline-variant);
    border-radius: 6px;
    padding: 6px 10px;
    display: flex;
    justify-content: space-between;
    cursor: pointer;
    font-size: 12px;
    color: var(--md-sys-color-on-surface);
  }

  .candidate-item:hover {
    background: var(--md-sys-color-surface-container-highest);
  }

  .env-table {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 120px;
    overflow-y: auto;
  }

  .env-row {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--md-sys-color-surface-container-high);
    padding: 4px 10px;
    border-radius: 6px;
    font-size: 12px;
  }

  .env-k {
    font-weight: 600;
    color: var(--md-sys-color-primary);
  }

  .env-v {
    flex: 1;
    color: var(--md-sys-color-on-surface);
  }

  .remove-env-btn {
    background: none;
    border: none;
    color: var(--md-sys-color-error);
    cursor: pointer;
    font-size: 12px;
  }

  .add-env-row {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }

  .add-env-row input {
    flex: 1;
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    color: var(--md-sys-color-on-surface);
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 12px;
  }

  .shortcuts-section {
    margin-top: 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .shortcut-actions {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .shortcut-btn {
    background: var(--md-sys-color-surface-container-high);
    border: 1px solid var(--md-sys-color-outline-variant);
    color: var(--md-sys-color-on-surface);
    border-radius: 8px;
    padding: 8px 12px;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    transition: background 150ms ease;
  }

  .shortcut-btn:hover {
    background: var(--md-sys-color-surface-container-highest);
  }
</style>
