<script>
  import GamePageView from '../src/views/GamePageView.svelte';
  const records = ['alpha', 'beta'].map((id) => ({ id, title: id, install_dir: `/fixture/${id}`, executable: 'game.exe', runner: 'native', environment: {}, arguments: [], is_running: false }));
  let game = $state(records[0]);
  export function selectGame(id) { game = records.find((record) => record.id === id); }
</script>
<GamePageView {game} library={records} platform="windows" runners={[{id:'native',name:'Native'}]} onBack={() => {}} onRefresh={async () => {}} />
