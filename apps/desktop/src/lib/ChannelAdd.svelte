<script lang="ts">
  import type { ProviderDefinition } from "$lib/desktop-api";
  import ProviderIcon from "$lib/ProviderIcon.svelte";
  export let providers: ProviderDefinition[];
  export let busy = false;
  export let onAdd: (id: string) => void;
  export let label = "添加";
</script>

<div class="channel-add-tools">
  {#if label}<span class="add-label">{label}</span>{/if}
  {#each providers as provider (provider.id)}
    <button class="channel-add" disabled={busy} aria-label={`添加${provider.name}`} on:click={() => onAdd(provider.id)}>
      <ProviderIcon providerId={provider.id} name={provider.name} size={22} />
      <span>{provider.name}</span>
    </button>
  {/each}
</div>

<style>
  .channel-add-tools { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
  .add-label { color: var(--muted); font-size: 12px; }
  .channel-add { display: inline-flex; align-items: center; gap: 7px; min-height: 36px; padding: 6px 12px; border: 1px solid var(--control-border); border-radius: 999px; background: transparent; color: var(--foreground); font: inherit; font-size: 12px; white-space: nowrap; cursor: pointer; }
  .channel-add:hover { background: var(--subtle); color: var(--foreground); }
  .channel-add:disabled { opacity: .5; cursor: default; }
  .channel-add:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
</style>
