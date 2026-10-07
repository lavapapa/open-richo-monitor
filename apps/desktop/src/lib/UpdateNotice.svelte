<script lang="ts">
  import type { UpdateStatus } from "$lib/desktop-api";

  export let status: UpdateStatus;
  export let mode: "banner" | "settings";
  export let checked = false;
  export let onCheck: () => void;
  export let onInstall: () => void;

  $: visible = status.phase === "available" || status.phase === "downloading" || status.phase === "installing";
  $: checking = status.phase === "checking";
  $: busy = checking || status.phase === "downloading" || status.phase === "installing";

  function bytes(value: number): string {
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    return `${(value / 1024 / 1024).toFixed(1)} MB`;
  }
</script>

{#if mode === "banner"}
  {#if visible}
    <section class="update-banner" aria-label="应用更新" role="status">
      <div class="copy">
        <strong>{status.phase === "installing" ? "正在安装更新…" : `发现新版本${status.version ? ` ${status.version}` : ""}`}</strong>
        {#if status.phase === "downloading"}
          <p>{status.total ? `${Math.min(100, Math.floor(status.downloaded / status.total * 100))}% · ` : "正在下载 · "}{bytes(status.downloaded)}{status.total ? ` / ${bytes(status.total)}` : ""}</p>
        {:else if status.phase === "installing"}
          <p>安装完成后应用将自动重启。</p>
        {:else}
          <p>安装时会短暂中断监控与通知连接，重启后恢复。</p>
        {/if}
      </div>
      {#if status.phase === "available"}<button class="primary" disabled={busy} on:click={onInstall}>下载并重启</button>{/if}
    </section>
  {/if}
{:else}
  <div class="settings-row update-settings">
    <div>
      <strong>应用更新</strong>
      {#if status.error}<p class="failure" role="alert">{status.error}</p>
      {:else if checking}<p role="status">正在检查更新…</p>
      {:else if status.phase === "available"}<p>发现新版本{status.version ? ` ${status.version}` : ""}。安装时会短暂中断监控与通知连接，重启后恢复。</p>
      {:else if status.phase === "downloading"}<p role="status">正在下载更新：{status.total ? `${Math.min(100, Math.floor(status.downloaded / status.total * 100))}% · ` : ""}{bytes(status.downloaded)}{status.total ? ` / ${bytes(status.total)}` : ""}</p>
      {:else if status.phase === "installing"}<p role="status">正在安装更新，应用即将重启。</p>
      {:else if checked}<p>当前已是最新版本。</p>
      {:else}<p>自动检查更新，每小时检查一次。</p>{/if}
      {#if status.phase === "available" && status.notes}<details><summary>更新内容</summary><pre>{status.notes}</pre></details>{/if}
    </div>
    <div class="actions">
      {#if status.phase === "available"}<button class="primary" disabled={busy} on:click={onInstall}>下载并重启</button>{/if}
      <button class="secondary" disabled={busy} on:click={onCheck}>{status.error ? "重试检查" : "检查更新"}</button>
    </div>
  </div>
{/if}

<style>
  .update-banner { display: flex; align-items: center; justify-content: space-between; gap: 16px; margin: 0 0 14px; padding: 12px 14px; border: 1px solid var(--accent); border-radius: 7px; background: var(--accent-soft); }
  .copy { min-width: 0; }
  strong { font-size: 13px; font-weight: 600; }
  p { margin: 3px 0 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
  .failure { color: var(--danger); }
  details { margin-top: 6px; font-size: 12px; }
  summary { color: var(--muted); cursor: pointer; }
  pre { max-width: min(680px, 70vw); margin: 6px 0 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; }
  .actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 8px; }
  @media (max-width: 700px) { .update-banner { align-items: flex-start; flex-direction: column; } }
</style>
