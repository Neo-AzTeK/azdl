<script>
  import { tick } from 'svelte';
  import GamePageView from '../../src/views/GamePageView.svelte';
  import { detailGameIdentity } from '../../src/detail_tabs.js';

  const fixtures = [
    { id: 'game-a', title: 'Installed game A', steam_app_id: 12345, install_dir: '/fixtures/a', runner: 'native', fix: 'None' },
    { id: 'game-b', title: 'Installed game B', steam_app_id: 12346, install_dir: '/fixtures/b', runner: 'native', fix: 'None' },
  ];
  let game = $state(fixtures[0]);
  let library = $state(fixtures);
  let platform = $state('windows');
  let results = $state([]);
  let running = $state(false);

  async function selectTab(name) {
    const label = [...document.querySelectorAll('#game-detail-tabs label')]
      .find(item => item.textContent.trim() === name);
    if (!label) throw new Error(`Missing tab: ${name}`);
    label.click();
    await tick();
  }

  function assertCards(context) {
    const cards = document.querySelectorAll('#section-patches .tool-card');
    const disabledButtons = document.querySelectorAll('#section-patches .tool-card button:disabled');
    const errors = document.getElementById('runtime-errors').textContent;
    if (cards.length !== 7 || disabledButtons.length !== 7 || errors) {
      throw new Error(`${context}: ${cards.length} cards, ${disabledButtons.length} disabled buttons; ${errors}`);
    }
    if (document.querySelector('#section-dlc') || document.querySelector('#runner-select')) {
      throw new Error(`${context}: another tab remained mounted`);
    }
    results = [...results, `PASS: ${context}`];
  }

  async function runChecks() {
    running = true;
    results = [];
    try {
      for (const host of ['windows', 'linux']) {
        platform = host;
        for (const fixture of fixtures) {
          game = fixture;
          await tick();
          for (const previous of ['DLC', 'Launch & Runner']) {
            await selectTab(previous);
            await selectTab('Patches');
            assertCards(`${host}, ${fixture.title}, ${previous} → Patches`);
            library = library.map(item => ({ ...item, size_bytes: (item.size_bytes || 0) + 1 }));
            game = { ...game, ...library.find(item => item.id === game.id), id: `${fixture.id}-normalized` };
            await tick();
            assertCards(`${host}, ${fixture.title}, background refresh after ${previous}`);
          }
        }
      }
      results = [...results, 'PASS: all 16 navigation and background refresh checks'];
    } catch (error) {
      results = [...results, `FAIL: ${error.message}`];
    } finally {
      running = false;
    }
  }
</script>

<button class="md3-btn tonal" onclick={runChecks} disabled={running}>Run navigation regression</button>
<pre aria-label="Regression results">{results.join('\n')}</pre>
{#key detailGameIdentity(game)}
  <GamePageView {game} {library} {platform} onBack={() => {}} settings={{ storage_drives: [] }} />
{/key}
