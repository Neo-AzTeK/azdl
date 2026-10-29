<script>
  import { untrack } from 'svelte';
  import { Switch } from 'm3-svelte';

  let { checked = false, disabled = false, onToggle } = $props();
  let selection = $state(false);

  // A new observed backend value wins over the switch's internal interaction state.
  $effect(() => { selection = checked; });
  $effect(() => {
    if (selection !== checked) {
      const requested = selection;
      selection = checked;
      if (!disabled) untrack(() => onToggle?.(requested));
    }
  });
</script>

<Switch bind:checked={selection} {disabled} />
