<script lang="ts">
  import { CircleArrowUp, Download, LoaderCircle, RotateCcw } from "@lucide/svelte";
  import type { UpdateStatus } from "$lib/desktop-api";

  export let status: UpdateStatus;
  export let mode: "sidebar" | "settings";
  export let checked = false;
  export let onCheck: () => void;
  export let onInstall: (immediate?: boolean) => void;
  export let onAutoChange: (enabled: boolean) => void = () => {};
  let confirmation: HTMLDialogElement;

  $: visible = ["available", "downloading", "waiting", "installing"].includes(status.phase);
  $: checking = status.phase === "checking";
  $: busy = checking || status.phase === "downloading" || status.phase === "installing";
  $: versionLabel = `新版本${status.version ? ` ${status.version}` : ""}`;
  $: percentage = status.total ? ` · ${Math.min(100, Math.floor(status.downloaded / status.total * 100))}%` : "";

  function bytes(value: number): string {
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    return `${(value / 1024 / 1024).toFixed(1)} MB`;
  }
</script>

{#if mode === "sidebar"}
  {#if visible}
    <div class="update-sidebar" aria-label="应用更新" role="status">
      {#if status.phase === "downloading"}
        <span class="compact" title={`${versionLabel} · ${bytes(status.downloaded)}${status.total ? ` / ${bytes(status.total)}` : ""}`}><LoaderCircle class="spinner" size={14} aria-hidden="true" /><span>{status.autoInstall ? "自动下载" : "正在下载"}{percentage}</span></span>
      {:else if status.phase === "installing"}
        <span class="compact" title="安装完成后应用将自动重启。"><LoaderCircle class="spinner" size={14} aria-hidden="true" /><span>正在更新…</span></span>
      {:else if status.phase === "waiting"}
        <button class="compact" aria-label="立即更新" title={`${versionLabel}已下载，监控与扫描空闲后安装；点击可立即更新。`} on:click={() => confirmation.showModal()}><CircleArrowUp size={14} aria-hidden="true" /><span>点击立即更新</span></button>
      {:else}
        <button class="compact" class:failure={!!status.error} aria-label={status.error ? "重试下载" : "下载更新"} title={status.error ? `${versionLabel} · ${status.error}` : `${versionLabel}，下载后等待监控空闲安装。`} on:click={() => onInstall(false)}>{#if status.error}<RotateCcw size={14} aria-hidden="true" />{:else}<Download size={14} aria-hidden="true" />{/if}<span>{status.error ? "更新失败 · 重试" : "新版本 · 下载"}</span></button>
      {/if}
    </div>
  {/if}
{:else}
  <div class="settings-row update-settings">
    <div>
      <strong>应用更新</strong>
      {#if status.error}<p class="failure" role="alert">{status.error}</p>{/if}
      {#if checking}<p role="status">正在检查更新…</p>
      {:else if status.phase === "available"}<p>发现新版本{status.version ? ` ${status.version}` : ""}。安装时会短暂中断监控与通知连接，重启后恢复。</p>
      {:else if status.phase === "downloading"}<p role="status">正在下载更新：{status.total ? `${Math.min(100, Math.floor(status.downloaded / status.total * 100))}% · ` : ""}{bytes(status.downloaded)}{status.total ? ` / ${bytes(status.total)}` : ""}</p>
      {:else if status.phase === "installing"}<p role="status">正在安装更新，应用即将重启。</p>
      {:else if status.phase === "waiting"}<p role="status">更新已下载，等待监控与扫描空闲后安装。</p>
      {:else if !status.error}<p>{checked ? "暂未发现更新。" : "自动检查更新，每小时检查一次。"}</p>{/if}
      {#if status.phase === "available" && status.notes}<details><summary>更新内容</summary><pre>{status.notes}</pre></details>{/if}
    </div>
    <div class="actions">
      {#if status.phase === "available"}<button class="primary" disabled={busy} on:click={() => onInstall(false)}>监控空闲时更新</button>{/if}
      {#if status.phase === "available" || status.phase === "waiting"}<button class="text-button" on:click={() => confirmation.showModal()}>立即更新</button>{/if}
      <button class="secondary" disabled={busy} on:click={onCheck}>{status.error ? "重试检查" : "检查更新"}</button>
    </div>
  </div>
  <div class="settings-row"><div><strong>监控空闲时自动更新</strong><p>发现更新后自动下载，监控停止、暂停或计划时段外再安装。</p></div><label class="switch"><input aria-label="监控空闲时自动更新" type="checkbox" checked={status.autoInstall} disabled={busy} on:change={(event) => onAutoChange(event.currentTarget.checked)} /></label></div>
{/if}

<dialog bind:this={confirmation} aria-labelledby={`update-confirm-${mode}`}>
  <h2 id={`update-confirm-${mode}`}>立即更新？</h2><p>安装将短暂中断监控与通知连接，完成后自动重启。</p>
  <div class="actions"><button class="secondary" on:click={() => confirmation.close()}>取消</button><button class="primary" on:click={() => { confirmation.close(); onInstall(true); }}>立即更新并重启</button></div>
</dialog>

<style>
  .update-sidebar { margin: 0 2px 8px; }
  .compact { display: flex; align-items: center; gap: 6px; width: 100%; min-height: 30px; padding: 5px 6px; font: inherit; font-size: 12px; line-height: 1.5; text-align: left; color: var(--muted); }
  button.compact { border: 0; border-radius: 5px; background: transparent; color: var(--accent-text); cursor: pointer; }
  button.compact:hover { background: var(--accent-soft); }
  button.compact:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .compact :global(svg) { flex: none; }
  .compact :global(.spinner) { animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .compact :global(.spinner) { animation: none; } }
  strong { font-size: 13px; font-weight: 600; }
  p { margin: 3px 0 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
  .failure { color: var(--danger); }
  details { margin-top: 6px; font-size: 12px; }
  summary { color: var(--muted); cursor: pointer; }
  pre { max-width: min(680px, 70vw); margin: 6px 0 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; }
  .actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 8px; }
  dialog { width: min(440px, calc(100vw - 32px)); padding: 24px; border: 1px solid var(--border); border-radius: 10px; background: var(--surface); color: var(--foreground); }
  dialog::backdrop { background: rgb(0 0 0 / 35%); }
  dialog h2 { margin: 0 0 12px; font-size: 20px; }
  dialog .actions { margin-top: 24px; }
</style>
