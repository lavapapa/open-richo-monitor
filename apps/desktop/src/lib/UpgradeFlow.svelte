<script lang="ts">
  import { onMount } from "svelte";
  import type { DesktopSnapshot } from "$lib/desktop-api";
  import { pendingUpgradeItems } from "$lib/upgrade-items";
  import UsageStatisticsConsent from "$lib/UsageStatisticsConsent.svelte";
  export let snapshot: DesktopSnapshot;
  export let onDone: () => Promise<void>;
  export let onDismiss: () => void = () => {};
  const items = pendingUpgradeItems(snapshot);
  let index = 0;
  let enabled = false;
  let saving = false;
  let error = "";
  let dialog: HTMLDialogElement;
  $: item = items[index];
  onMount(() => dialog.showModal());
  async function confirm() {
    if (saving) return;
    saving = true; error = "";
    try {
      await item.save(enabled);
      if (index + 1 < items.length) { index++; enabled = false; }
      else await onDone();
    } catch (cause) { error = cause instanceof Error ? cause.message : "选择未保存，请重试。"; }
    finally { saving = false; }
  }
</script>

<dialog bind:this={dialog} aria-labelledby="upgrade-title" on:cancel={(event) => { event.preventDefault(); if (!saving) onDismiss(); }}>
  <header><span class="progress">{index + 1}/{items.length}</span><h2 id="upgrade-title">{item.title}</h2></header>
  <div class="content">
    {#if item.id === "configuration"}
      <figure role="img" aria-label="取消 GR IV 提醒的通知示例">
        <div class="notification-header"><img src="/brand/rm-wordmark.svg" alt="" /><strong>RichoMonitor</strong></div>
        <div class="notification-body"><strong>监控产品变更：减少【RICOH GR IV】</strong><span>当前监控范围：官翻品 RICOH GR III、官翻品 GR IIIx、RICOH GR III Diary Edition 日记版、RICOH GR IIIx Urban Edition 都市版、官翻品 RICOH GR III HDF、官翻品 GR IIIx HDF、官翻品 GR IV HDF</span><time>2026-10-13 09:42:16.028</time></div>
      </figure>
      <p>调整商品、计划或检查间隔后，向现有通知渠道发送变更提醒。</p>
    {:else if item.id === "usage"}
      <UsageStatisticsConsent showChoice={false} />
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
  <footer><label><input type="checkbox" bind:checked={enabled} disabled={saving} />{item.choice}</label><button class="primary" disabled={saving} on:click={confirm}>{saving ? "保存中…" : index + 1 === items.length ? "完成" : "下一步"}</button></footer>
</dialog>

<style>
  dialog { width: 560px; height: 400px; max-width: calc(100vw - 32px); max-height: calc(100dvh - 32px); box-sizing: border-box; padding: 26px; border: 1px solid var(--border); border-radius: 14px; background: var(--canvas); color: var(--foreground); box-shadow: 0 16px 60px #0002; }
  dialog[open] { display: flex; flex-direction: column; }
  dialog::backdrop { background: #0006; }
  header { flex: none; }
  .progress { font-size: 12px; color: var(--muted); font-variant-numeric: tabular-nums; }
  h2 { font-size: 21px; margin: 8px 0 18px; letter-spacing: -.3px; }
  .content { flex: 1; min-height: 0; overflow-y: auto; }
  figure { margin: 0 0 14px; padding: 15px; border-radius: 10px; background: var(--surface); border: 1px solid var(--border); }
  .notification-header { display: flex; align-items: center; gap: 8px; font-size: 12px; margin-bottom: 9px; }
  img { width: 22px; height: 18px; }
  .notification-body { display: grid; gap: 5px; font-size: 12px; line-height: 1.6; }
  .notification-body strong { font-weight: 550; }
  time { font-size: 11px; color: var(--muted); }
  p { margin: 0; color: var(--muted); font-size: 12px; line-height: 1.6; }
  footer { flex: none; display: flex; justify-content: space-between; align-items: center; gap: 15px; padding-top: 18px; }
  label { display: flex; align-items: center; gap: 8px; font-size: 13px; cursor: pointer; }
  input { width: 15px; height: 15px; margin: 0; accent-color: var(--accent); }
  button { padding: 9px 17px; border: 1px solid var(--accent); border-radius: 7px; background: var(--accent); color: white; font: inherit; font-size: 13px; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  input:focus-visible, button:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .error { color: var(--danger); margin-top: 12px; }
</style>
