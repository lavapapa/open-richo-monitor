<script lang="ts">
  import Coffee from "@lucide/svelte/icons/coffee";
  import Star from "@lucide/svelte/icons/star";
  import { desktopApi } from "$lib/desktop-api";
  export let url: string | null;
  let error = "";
  async function open(address: string) {
    try { await desktopApi.openExternalUrl(address); error = ""; }
    catch (cause) { error = cause instanceof Error ? cause.message : "打开网页失败，请重试。"; }
  }
</script>

<div class="support">
  <p class="caption">RichoMonitor 是一个完全开源免费的软件，如果帮到了你，欢迎 star/打赏！</p>
  <div class="actions">
    <button class="star-button" on:click={() => open("https://github.com/lavapapa/open-richo-monitor")}><Star size={17} aria-hidden="true" />GitHub Star</button>
    {#if url}<button class="support-button" on:click={() => url && open(url)}><Coffee size={18} aria-hidden="true" />打赏作者</button>{/if}
  </div>
  {#if error}<p role="alert">{error}</p>{/if}
</div>

<style>
  .support { text-align: center; }
  .caption { margin: 0 0 14px; color: var(--muted); font-size: 13px; line-height: 1.65; text-wrap: pretty; }
  .actions { display: flex; align-items: center; justify-content: center; gap: 16px; flex-wrap: wrap; }
  button { display: inline-flex; gap: 8px; align-items: center; justify-content: center; min-height: 36px; font: inherit; font-size: 13px; cursor: default; }
  .star-button { padding: 9px 4px; border: 0; border-radius: 8px; background: transparent; color: var(--foreground); }
  .star-button :global(svg) { color: #ae791e; }
  .star-button:hover :global(svg) { fill: #f7cd67; }
  .support-button { padding: 9px 16px; border: 1px solid #e7aa64; border-radius: 12px; background: linear-gradient(180deg, #ffe3ac, #ffd18b); color: #653914; font-weight: 600; box-shadow: inset 0 1px 0 #fff9, 0 2px 0 #9a58191c; }
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  [role="alert"] { color: var(--danger); font-size: 13px; }
  @media (prefers-reduced-motion: reduce) { button, .star-button :global(svg) { transition: none; } .star-button:hover :global(svg), .support-button:hover, button:active { transform: none; } }
</style>
