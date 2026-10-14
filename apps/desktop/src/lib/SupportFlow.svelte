<script lang="ts">
  import { onMount } from "svelte";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import SupportButton from "$lib/SupportButton.svelte";
  import { desktopApi } from "$lib/desktop-api";
  import support from "$lib/support.json";
  import appInfo from "../../src-tauri/tauri.conf.json";
  export let onClose: () => void;
  export let fundingUrl: string | null = support.fundingUrl;
  export let feedbackUrl = support.feedbackUrl;
  export let appVersion = appInfo.version;
  export let initialStep: "rating" | "feedback" = "rating";
  let dialog: HTMLDialogElement;
  let step: "rating" | "positive" | "feedback" = initialStep;
  let loaded = false;
  let error = "";
  $: formLink = feedbackLink(feedbackUrl, appVersion);
  $: embedLink = formLink.replace("/r/", "/embed/") + "&hideTitle=1&alignLeft=1";
  onMount(() => dialog.showModal());
  function feedbackLink(address: string, version: string) {
    const url = new URL(address);
    url.searchParams.set("version", version);
    const agent = typeof navigator === "undefined" ? "" : navigator.userAgent;
    url.searchParams.set("platform", /Windows/i.test(agent) ? "windows" : /Macintosh|Mac OS/i.test(agent) ? "macos" : "linux");
    return url.toString();
  }
  async function openFeedback() {
    error = "";
    try { await desktopApi.openExternalUrl(formLink); }
    catch (cause) { error = cause instanceof Error ? cause.message : "打开反馈网页失败，请重试。"; }
  }
</script>

<dialog bind:this={dialog} class:feedback={step === "feedback"} aria-labelledby="support-title" on:close={onClose}>
  {#if step === "rating"}
    <h2 id="support-title">恭喜买到心仪的相机！</h2>
    <p>RichoMonitor 用起来怎么样？</p>
    <div class="choices"><button on:click={() => step = "positive"}>好评</button><button on:click={() => step = "feedback"}>有一些意见</button></div>
  {:else if step === "positive"}
    <h2 id="support-title">谢谢你的肯定</h2>
    <div class="choices"><SupportButton url={fundingUrl} /></div>
  {:else if step === "feedback"}
    <h2 id="support-title">告诉我你的意见</h2>
    <div class="form">
      {#if !loaded}<div class="loading" role="status"><LoaderCircle size={20} aria-hidden="true" />正在加载反馈表单…</div>{/if}
      <iframe src={embedLink} title="RichoMonitor 反馈表单" on:load={() => loaded = true}></iframe>
    </div>
  {/if}
  {#if error}<p role="alert">{error}</p>{/if}
  <footer>{#if step === "feedback"}<button class="skip" on:click={openFeedback}>打开网页填写</button>{/if}<button class="skip" on:click={() => dialog.close()}>{step === "feedback" ? "关闭" : "跳过"}</button></footer>
</dialog>

<style>
  dialog { width: min(440px, calc(100vw - 40px)); box-sizing: border-box; padding: 28px; border: 1px solid var(--border); border-radius: 16px; background: var(--canvas); color: var(--foreground); box-shadow: 0 16px 60px #0002; }
  dialog::backdrop { background: #0006; }
  h2 { margin: 0 0 12px; font-size: 20px; }
  p { color: var(--muted); line-height: 1.6; }
  .choices { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; margin: 22px 0; }
  button { display: inline-flex; align-items: center; gap: 8px; border: 1px solid var(--border); background: var(--surface); color: inherit; border-radius: 8px; padding: 9px 13px; font: inherit; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  footer { display: flex; justify-content: flex-end; gap: 12px; margin-top: 24px; }
  .skip { border-color: transparent; background: transparent; color: var(--muted); }
  .feedback { width: min(660px, calc(100vw - 40px)); }
  .feedback footer { justify-content: space-between; margin-top: 12px; }
  .form { position: relative; height: min(560px, calc(100dvh - 190px)); min-height: 220px; border-radius: 8px; overflow: hidden; background: white; }
  iframe { width: 100%; height: 100%; border: 0; }
  .loading { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; gap: 10px; background: white; color: #59616b; pointer-events: none; }
  .loading :global(svg) { animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .loading :global(svg) { animation: none; } }
  [role="alert"] { color: var(--danger); }
</style>
