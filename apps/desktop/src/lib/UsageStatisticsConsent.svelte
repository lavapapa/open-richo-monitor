<script lang="ts">
  import { desktopApi } from "$lib/desktop-api";
  export let enabled = false;
  export let busy = false;
  export let showChoice = true;
  export let compact = false;
  export let onChange: (enabled: boolean) => void = () => {};
  let linkError = "";
  async function openService(event: MouseEvent) {
    if (desktopApi.previewMode) return;
    event.preventDefault();
    linkError = "";
    try { await desktopApi.openExternalUrl("https://posthog.com/"); }
    catch (error) { linkError = error instanceof Error ? error.message : "链接未打开，请重试。"; }
  }
</script>

<section class="consent" class:compact aria-label="使用统计说明">
  {#if !compact}
  <div class="columns">
    <div><h4><span aria-hidden="true">✓</span> 会发送</h4><ul>
      <li>随机安装标识，用于统计活跃安装</li>
      <li>系统、处理器架构与 App 版本，<span class="purpose">用于解决 bug</span></li>
      <li>监控发起次数、商品和通知对象数量</li>
    </ul></div>
    <div><h4><span aria-hidden="true">−</span> 不会发送</h4><ul>
      <li>群名、聊天 ID、账号和通知凭据</li>
      <li>监控记录</li>
      <li>截图、录屏、点击轨迹和设备序列号</li>
    </ul></div>
  </div>
  <div class="service"><span>统计服务</span><a href="https://posthog.com/" target="_blank" rel="noopener noreferrer" on:click={openService}>PostHog <span aria-hidden="true">↗</span></a></div>
  {/if}
  {#if showChoice}<label class="choice"><input type="checkbox" aria-label="分享使用统计（可选）" checked={enabled} disabled={busy} on:change={(event) => { event.currentTarget.checked = enabled; onChange(!enabled); }} /><span>同意向 {#if compact}<a href="https://posthog.com/" target="_blank" rel="noopener noreferrer" on:click={openService}>PostHog</a>{:else}PostHog{/if} 分享使用统计，帮助改进 RichoMonitor。</span></label>{/if}
  {#if linkError}<p role="alert">{linkError}</p>{/if}
</section>

<style>
  .consent { width: 100%; text-align: left; color: var(--foreground); }
  .columns { display: grid; grid-template-columns: 1fr 1fr; gap: 22px; margin: 10px 0 18px; }
  h4 { margin: 0 0 9px; font-size: 12px; font-weight: 600; }
  h4 span { margin-right: 4px; color: var(--accent); }
  .columns > div:last-child h4 span { color: var(--muted); }
  ul { list-style: none; padding: 0; margin: 0; display: grid; gap: 7px; }
  li { color: var(--muted); font-size: 12px; line-height: 1.6; text-wrap: pretty; }
  .purpose { white-space: nowrap; }
  .service { display: flex; align-items: center; gap: 10px; font-size: 12px; }
  .service > span { color: var(--muted); }
  a { color: var(--foreground); text-decoration: none; font-weight: 550; }
  a:hover { color: var(--accent); text-decoration: underline; }
  a:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .choice { display: flex; align-items: center; gap: 9px; margin-top: 18px; font-size: 13px; line-height: 1.6; cursor: pointer; }
  .compact .choice { margin: 0; color: var(--muted); font-size: 12px; }
  input { width: 15px; height: 15px; flex: none; margin: 0; accent-color: var(--accent); }
  [role="alert"] { font-size: 12px; color: var(--danger); }
  @media (max-width: 560px) { .columns { grid-template-columns: 1fr; gap: 16px; } }
</style>
