<script lang="ts">
  import { onMount } from "svelte";
  import Star from "@lucide/svelte/icons/star";
  import SupportButton from "$lib/SupportButton.svelte";
  import { desktopApi } from "$lib/desktop-api";
  import support from "$lib/support.json";
  export let onClose: () => void;
  export let fundingUrl: string | null = support.fundingUrl;
  export let feedbackUrl: string | null = support.feedbackUrl;
  export let initialStep: "rating" | "feedback" = "rating";
  let dialog: HTMLDialogElement;
  let step: "rating" | "positive" | "feedback" | "sent" = initialStep;
  let content = "";
  let busy = false;
  let error = "";
  let copied = false;
  onMount(() => dialog.showModal());
  async function submit() {
    if (!content.trim() || busy) return;
    busy = true; error = "";
    try { await desktopApi.submitFeedback(content); step = "sent"; }
    catch (cause) { error = cause instanceof Error ? cause.message : "反馈未提交，请重试。"; }
    finally { busy = false; }
  }
  async function copy() {
    try { await navigator.clipboard.writeText(content); copied = true; error = ""; }
    catch { error = "复制失败，请选择文字后手动复制。"; }
  }
  async function star() {
    try { await desktopApi.openExternalUrl("https://github.com/lavapapa/open-richo-monitor"); }
    catch (cause) { error = cause instanceof Error ? cause.message : "打开项目主页失败，请重试。"; }
  }
</script>

<dialog bind:this={dialog} aria-labelledby="support-title" on:cancel={(event) => { if (busy) event.preventDefault(); }} on:close={onClose}>
  {#if step === "rating"}
    <h2 id="support-title">恭喜买到心仪的相机！</h2>
    <p>RichoMonitor 用起来怎么样？</p>
    <div class="choices"><button on:click={() => step = "positive"}>好评</button><button on:click={() => step = "feedback"}>有一些意见</button></div>
  {:else if step === "positive"}
    <h2 id="support-title">谢谢你的肯定</h2>
    <p>欢迎给项目点一个 Star，或支持后续维护。</p>
    <div class="choices">{#if fundingUrl}<SupportButton url={fundingUrl} />{/if}<button on:click={star}><Star size={17} aria-hidden="true" />在 GitHub 点 Star</button></div>
  {:else if step === "feedback"}
    <h2 id="support-title">告诉我你的意见</h2>
    <textarea aria-label="意见" bind:value={content} rows="5" maxlength="4000" placeholder="哪里让你困扰？你希望怎样改进？" disabled={busy}></textarea>
    {#if feedbackUrl}<p class="small">提交内容及应用版本、系统类型会发给开发者，帮助改进体验。</p><button on:click={submit} disabled={busy || !content.trim()}>{busy ? "正在提交…" : "提交反馈"}</button>
    {:else}<p class="small">反馈服务尚未开通，你可以先复制意见。</p><button on:click={copy} disabled={!content.trim()}>{copied ? "已复制" : "复制反馈"}</button>{/if}
  {:else}<h2 id="support-title">反馈已收到，谢谢你。</h2>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  <footer><button class="skip" disabled={busy} on:click={() => dialog.close()}>{step === "sent" ? "关闭" : "跳过"}</button></footer>
</dialog>

<style>
  dialog { width: min(440px, calc(100vw - 40px)); box-sizing: border-box; padding: 28px; border: 1px solid var(--border); border-radius: 16px; background: var(--canvas); color: var(--foreground); box-shadow: 0 16px 60px #0002; }
  dialog::backdrop { background: #0006; }
  h2 { margin: 0 0 12px; font-size: 20px; }
  p { color: var(--muted); line-height: 1.6; }
  .choices { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; margin: 22px 0; }
  button { display: inline-flex; align-items: center; gap: 8px; border: 1px solid var(--border); background: var(--surface); color: inherit; border-radius: 8px; padding: 9px 13px; font: inherit; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  button:focus-visible, textarea:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  footer { display: flex; justify-content: flex-end; margin-top: 24px; }
  .skip { border-color: transparent; background: transparent; color: var(--muted); }
  textarea { width: 100%; box-sizing: border-box; padding: 10px; border: 1px solid var(--border); background: var(--surface); color: inherit; border-radius: 8px; font: inherit; resize: vertical; }
  .small { font-size: 12px; }
  [role="alert"] { color: var(--danger); }
</style>
