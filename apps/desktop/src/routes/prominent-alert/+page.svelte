<script lang="ts">
  import { onMount, tick } from "svelte";
  import appInfo from "../../../src-tauri/tauri.conf.json";
  import { desktopApi, prominentImageSrc, type ProminentAlert } from "$lib/desktop-api";
  import "$lib/theme.css";
  let alert: ProminentAlert | null = null;
  let error = "";
  let closing = false;
  let presentationId = 0;
  let closeButton: HTMLButtonElement;
  let productImage: HTMLImageElement;
  $: image = alert ? prominentImageSrc(alert) ?? alert.imageUrl : null;
  $: test = !!alert && alert.eventId < 0;
  async function receive(next: ProminentAlert & { presentationId: number }) {
    presentationId = next.presentationId;
    alert = next;
    closing = false;
    error = "";
    await tick();
    // 本地图片解码属于实际渲染；远端图片保持异步，避免网络阻塞提醒。
    if (next.imagePath && productImage?.decode) {
      try { await productImage.decode(); } catch { await tick(); }
    }
    if (presentationId !== next.presentationId) return;
    closeButton?.focus();
    try { await desktopApi.showProminentAlert(next.eventId, next.presentationId); }
    catch (cause) { if (presentationId === next.presentationId) error = cause instanceof Error ? cause.message : "显示提醒失败。"; }
  }
  async function dismiss() {
    if (!alert || closing) return;
    const current = alert;
    const currentPresentation = presentationId;
    closing = true;
    try { await desktopApi.dismissProminentAlert(current.eventId); if (desktopApi.previewMode && window.parent !== window) window.parent.dispatchEvent(new Event("rm-preview-alert-close")); }
    catch (cause) { if (presentationId === currentPresentation) { error = cause instanceof Error ? cause.message : "关闭提醒失败，请重试。"; closing = false; } }
  }
  onMount(() => {
    let stop: (() => void) | undefined;
    let mounted = true;
    void desktopApi.subscribeProminentAlert((next) => { void receive(next); })
      .then((unsubscribe) => { if (mounted) stop = unsubscribe; else unsubscribe(); })
      .catch((cause) => { error = cause instanceof Error ? cause.message : "准备提醒失败。"; });
    return () => { mounted = false; stop?.(); };
  });
</script>

<svelte:head><title>库存提醒 · RM</title></svelte:head>
<svelte:window on:keydown={(event) => { if (["Escape", " ", "Enter"].includes(event.key)) { event.preventDefault(); void dismiss(); } }} />
<main class="alert-screen">
  <div class="alert-card">
  <header><img src="/brand/rm-wordmark.svg" alt="RM" /><button bind:this={closeButton} aria-label="关闭" title="关闭（Esc、空格或回车）" on:click={dismiss} disabled={closing || !alert}>ESC 关闭</button></header>
  {#if alert}
    <section aria-labelledby="alert-title">
      <div class="product-visual">{#if image}<img bind:this={productImage} src={image} alt={alert.name} on:error={() => image = null} />{:else}<span>商品图片待更新</span>{/if}</div>
      <div class="product-information">
        <p class="eyebrow">库存提醒</p>
        <h1 id="alert-title">{alert.name}</h1>
        <div class="numbers"><div><small>商品价格</small><strong>{alert.price ? `¥${alert.price}` : "待更新"}</strong></div><div><small>库存数量</small><strong>{alert.stock}</strong></div></div>
        <div class="actions"><button class="dismiss" on:click={dismiss} disabled={closing}>完成</button></div>
      </div>
    </section>
    <footer><span>{appInfo.productName}</span>{#if !test}<time>{new Date(alert.at).toLocaleString("zh-CN", { timeZone: "Asia/Shanghai", hour12: false })} · 北京时间</time>{/if}</footer>
  {:else}<section class="loading"><p>{error ? "提醒暂时无法读取" : "正在读取商品提醒…"}</p></section>{/if}
  {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
</main>

<style>
  :global(html), :global(body) { margin: 0; background: transparent; font-family: -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif; }
  :global(*) { box-sizing: border-box; }
  .alert-screen { height: 100dvh; display: grid; place-items: center; padding: clamp(16px, 4vw, 64px); background: #f5f6f8; color: #271b13; font-variant-numeric: tabular-nums; }
  .alert-card { display: flex; flex-direction: column; width: min(100%, 1280px); max-height: 100%; overflow: auto; padding: clamp(20px, 3vw, 48px); background: #ffd526; border-radius: 22px; box-shadow: 0 24px 90px rgb(0 0 0 / 24%); }
  header { display: flex; align-items: center; gap: 24px; font-size: 14px; }
  header img { width: 72px; }
  header button { margin-left: auto; border: 0; padding: 8px 12px; background: transparent; color: inherit; border-radius: 6px; }
  section { flex: 1; width: 100%; max-width: 1360px; margin: 32px auto; display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); align-items: center; gap: clamp(24px, 5vw, 80px); }
  .product-visual { height: min(42dvh, 440px); min-width: 0; display: flex; align-items: center; justify-content: center; padding: 16px; background: #fff; border-radius: 10px; }
  .product-visual img { display: block; min-width: 0; min-height: 0; width: 100%; height: 100%; object-fit: contain; }
  .eyebrow { margin: 0 0 20px; font-weight: 650; font-size: 20px; color: #a22324; }
  h1 { margin: 0; font-size: clamp(28px, 4vw, 58px); line-height: 1.15; letter-spacing: -.035em; text-wrap: balance; overflow-wrap: anywhere; }
  .numbers { display: flex; flex-wrap: wrap; gap: 36px; margin: 32px 0 24px; }
  .numbers small { display: block; margin-bottom: 8px; font-size: 15px; }
  .numbers strong { display: block; font-size: clamp(24px, 3vw, 42px); letter-spacing: -.04em; }
  .actions { display: flex; flex-wrap: wrap; gap: 12px; margin-top: 28px; }
  button { font: inherit; cursor: pointer; transition: opacity 160ms; }
  button:hover { opacity: .8; }
  button:focus-visible { outline: 3px solid #271b13; outline-offset: 4px; }
  button:disabled { opacity: .5; cursor: default; }
  .actions button { border-radius: 4px; padding: 16px 22px; }
  .dismiss { color: #fffdf4; background: #b6252e; border: 0; min-width: 100px; font-weight: 600; }
  footer { display: flex; justify-content: space-between; flex-wrap: wrap; gap: 10px; font-size: 12px; color: #685724; }
  .error { color: #9c1820; background: #fffdf4; padding: 16px; }
  .loading { display: block; }
  @media (max-width: 760px) { section { grid-template-columns: minmax(0, 1fr); } .product-visual { height: 30dvh; } header { gap: 12px; } }
</style>
