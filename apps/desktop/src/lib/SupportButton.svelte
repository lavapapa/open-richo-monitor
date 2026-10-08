<script lang="ts">
  import Coffee from "@lucide/svelte/icons/coffee";
  import { desktopApi } from "$lib/desktop-api";
  export let url: string;
  let error = "";
  async function open() {
    try { await desktopApi.openExternalUrl(url); error = ""; }
    catch (cause) { error = cause instanceof Error ? cause.message : "打开爱发电失败，请重试。"; }
  }
</script>

<button class="support-button" on:click={open}><Coffee size={18} aria-hidden="true" />打赏作者</button>
{#if error}<p role="alert">{error}</p>{/if}

<style>
  .support-button { display: inline-flex; gap: 8px; align-items: center; justify-content: center; border: 1px solid #d89052; border-radius: 999px; padding: 10px 16px; background: linear-gradient(115deg, #ffe2a8, #ffd4ba 52%, #e9cfff); color: #4c2936; font: inherit; font-weight: 650; cursor: pointer; box-shadow: 0 2px 7px #c6844420; }
  .support-button:hover { filter: brightness(1.04); }
  .support-button:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  p { color: var(--danger); font-size: 12px; }
</style>
