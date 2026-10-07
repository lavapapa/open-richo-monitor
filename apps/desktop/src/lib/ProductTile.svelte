<script lang="ts">
  import ProminentIcon from "$lib/ProminentIcon.svelte";
  import FlipCount from "$lib/FlipCount.svelte";
  import { formatMonitoringDuration, productImageSrc, type ProductRecord } from "$lib/desktop-api";

  export let product: ProductRecord;
  export let compact = false;
  export let selected = false;
  export let pending = false;
  export let prominentPending = false;
  export let onProminent: (enabled: boolean) => void = () => {};
  export let onOpen: (origin: EventTarget | null) => void;
  export let onToggle: (enabled: boolean) => void;
  export let onError: (error: string) => void;

  $: image = productImageSrc(product);
  $: state = product.observation?.isShow === 1 ? "已上架" : product.observation?.isShow === 0 ? "未上架" : "尚无成功检查";
  $: availability = product.observation?.availability === "in_stock" ? "有货" : product.observation?.availability === "out_of_stock" ? "无货" : "等待检查";
</script>

<article class="product-tile" class:compact class:selected class:failed={!!product.runtimeError}>
  <button class="product-photo" aria-label={`查看${product.name}详情`} on:click={(event) => onOpen(event.currentTarget)}>
    {#if image}<img src={image} alt={product.name} loading="lazy" />{:else}<span class="photo-empty">图片待更新</span>{/if}
  </button>
  <div class="product-copy">
    <div class="product-title-row">
    <button class="product-name" on:click={(event) => onOpen(event.currentTarget)}>{product.name}</button>
    {#if compact}<button class="product-count" title={`今日检查 ${(product.todayCheckCount ?? 0).toLocaleString("zh-CN")} 次，查看记录`} aria-label={`今日检查 ${product.todayCheckCount ?? 0} 次，查看记录`} on:click={(event) => onOpen(event.currentTarget)}><FlipCount count={product.todayCheckCount ?? 0} /></button>{/if}
    </div>
    <div class="product-summary">
    <div class="product-availability">
    {#if product.enabled}<button class="prominent-toggle" class:enabled={product.prominentAlert} aria-label={`突出提醒${product.name}`} aria-pressed={product.prominentAlert} title={product.prominentAlert ? "关闭突出提醒" : "开启突出提醒"} disabled={prominentPending} on:click={() => onProminent(!product.prominentAlert)}><ProminentIcon /></button>{/if}
    <p class="product-state">{#if product.runtimeError}<button class="error-link product-error-link" on:click={() => onError(product.runtimeError!)}>检查失败</button>{:else}<span class:positive={product.observation?.isShow === 1}>{state}</span>{#if product.observation && product.observation.stock !== null}<span aria-hidden="true"> · </span><span class:positive={product.observation.availability === "in_stock"}>{availability} {product.observation.stock}</span>{/if}{/if}</p>
    </div>
    {#if product.metadata?.price}<p class="product-price">¥{product.metadata.price}{product.metadata.unitName ? ` / ${product.metadata.unitName}` : ""}</p>{/if}
    </div>
    {#if product.runtimeError}<pre class="product-error-detail" title="原始错误内容">{product.runtimeError}</pre>{/if}
    {#if !compact}
      <div class="product-bottom"><span class="monitor-state"><i class:active={product.enabled}></i>{product.enabled ? "监控中" : "未监控"}</span><button class="monitor-action" class:active={product.enabled} aria-pressed={product.enabled} aria-label={`${product.enabled ? "取消监控" : "添加监控"}${product.name}`} disabled={pending} on:click={() => onToggle(!product.enabled)}>{product.enabled ? "取消监控" : "添加监控"}</button></div>
      <small class="product-id">Product ID {product.productId}</small>
    {/if}
  </div>
  {#if compact}<div class="product-popover">
    <strong>今日检查 {product.todayCheckCount ?? 0} 次</strong>
    <span>成功 {product.todaySuccessCount ?? 0} · 失败 {product.todayFailureCount ?? 0}</span>
    <span>累计检查 {product.checkCount} 次</span>
    <span>累计监控 {formatMonitoringDuration(product.monitoringMs)}</span>
    {#if product.observation?.checkedAt}<span title="最近成功检查">最近成功 {new Date(product.observation.checkedAt).toLocaleString("zh-CN", { timeZone: "Asia/Shanghai", month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3, hour12: false })}</span>{/if}
  </div>{/if}
</article>

<style>
  .product-tile { position: relative; min-width: 0; padding: 8px 10px 10px; background: var(--canvas); }
  .product-photo { position: relative; display: block; width: 100%; height: 134px; padding: 0; border: 0; border-radius: var(--photo-radius); overflow: hidden; background: var(--photo-surface); }
  .product-photo img { position: absolute; inset: 6px; display: block; width: calc(100% - 12px); height: calc(100% - 12px); object-fit: contain; mix-blend-mode: var(--photo-blend); }
  .product-photo:has(.photo-empty) { background: var(--subtle); }
  .product-tile.selected:not(.compact) .product-photo { border-bottom: 2px solid var(--accent); }
  .photo-empty { position: absolute; inset: 0; display: grid; place-items: center; color: var(--muted); font-size: 13px; }
  .product-copy { min-width: 0; }
  .product-title-row { display: flex; align-items: flex-start; gap: 5px; height: 36.4px; margin-top: 6px; }
  .product-name { display: -webkit-box; flex: 1; min-width: 0; padding: 0; border: 0; overflow: hidden; background: transparent; color: var(--foreground); text-align: left; font-size: 13px; line-height: 1.4; font-weight: 600; line-clamp: 2; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
  .product-name:hover { color: var(--accent); }
  .product-summary { display: flex; align-items: baseline; justify-content: space-between; flex-wrap: wrap; gap: 3px 8px; margin-top: 4px; }
  .product-availability { display: inline-flex; align-items: center; gap: 4px; }
  .product-state { margin: 0; color: var(--muted); font-size: 12px; }
  .product-price { margin: 0 0 0 auto; color: var(--foreground); font-size: 11px; font-weight: 550; white-space: nowrap; }
  .positive { color: var(--positive); }
  .error-link { padding: 0; border: 0; background: transparent; color: var(--danger); text-decoration: underline; text-underline-offset: 3px; }
  .product-error-detail { margin: 6px 0 0; padding: 6px 8px; max-height: 80px; overflow: auto; color: var(--danger); background: var(--danger-soft); border-radius: 4px; white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; font: 10px/1.5 ui-monospace, monospace; }
  .product-count { display: inline-flex; align-items: center; flex: none; padding: 1px 0; border: 0; background: transparent; color: var(--muted); font-size: 11px; white-space: nowrap; }
  .product-count:hover { color: var(--foreground); }
  .prominent-toggle { display: inline-grid; place-items: center; flex: none; width: 22px; height: 22px; padding: 0; border: 0; border-radius: 3px; color: var(--muted); background: transparent; }
  .prominent-toggle:hover { background: var(--subtle); }
  .prominent-toggle.enabled { color: var(--accent-text); background: var(--accent-soft); }
  .prominent-toggle:disabled { opacity: .5; }
  .product-bottom { display: flex; align-items: center; justify-content: space-between; margin-top: 7px; color: var(--muted); font-size: 11px; }
  .monitor-state { display: inline-flex; align-items: center; gap: 9px; }
  .monitor-state i { width: 9px; height: 9px; border-radius: 50%; background: var(--muted); }
  .monitor-state i.active { background: var(--positive); }
  .monitor-action { padding: 0; border: 0; background: transparent; color: var(--muted); }
  .monitor-action:hover { color: var(--accent); }
  .monitor-action.active { color: var(--muted); }
  .product-id { display: block; margin-top: 5px; color: var(--muted); font-size: 10px; }
  .product-popover { position: absolute; z-index: 5; left: 12px; right: 12px; top: 70%; display: none; pointer-events: none; padding: 10px 12px; border: 1px solid var(--border); border-radius: 6px; background: var(--surface); box-shadow: 0 10px 25px rgb(0 0 0 / 12%); color: var(--muted); font-size: 12px; }
  .product-popover strong, .product-popover span { display: block; margin-bottom: 5px; }
  .product-popover strong { color: var(--foreground); }
  .product-tile.compact:not(.selected):has(.product-count:hover) .product-popover, .product-tile.compact:not(.selected):has(.product-photo:hover) .product-popover, .product-tile.compact:not(.selected):focus-within .product-popover { display: block; }
  .product-tile:hover, .product-tile:focus-within { z-index: 2; }
  @media (prefers-reduced-motion: reduce) { .product-tile * { transition: none !important; } }
</style>
