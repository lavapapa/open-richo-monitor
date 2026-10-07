<script lang="ts">
  import { tick } from "svelte";
  import { type DesktopSnapshot, type ProductRecord } from "$lib/desktop-api";
  import ProductChoice from "$lib/ProductChoice.svelte";
  import ChannelAdd from "$lib/ChannelAdd.svelte";
  import ProviderIcon from "$lib/ProviderIcon.svelte";
  import ChannelStatus from "$lib/ChannelStatus.svelte";
  import RecipientChip from "$lib/RecipientChip.svelte";
  import CompletionConfetti from "$lib/CompletionConfetti.svelte";

  export let snapshot: DesktopSnapshot;
  export let busy = false;
  export let onProducts: (ids: string[]) => Promise<boolean>;
  export let onSystemEnable: () => Promise<void>;
  export let onSystemTest: () => Promise<void>;
  export let onAddChannel: (providerId: string) => void;
  export let onChannelTest: (id: string) => void;
  export let onChannelEdit: (channel: DesktopSnapshot["channels"][number]) => void = () => {};
  export let onChannelToggle: (id: string, enabled: boolean) => void;
  export let onComplete: (loginStart: boolean, autoStartMonitoring: boolean) => Promise<void>;
  export let onCancel: () => void;

  const isRefurbishedGr = (product: ProductRecord) => /官翻/.test(product.name) && /GR\s*(III|IV)/i.test(product.name);
  const existingProductIds = snapshot.products.filter((product) => product.enabled).map((product) => product.productId);
  const defaultProductIds = [...new Map([...snapshot.catalog, ...snapshot.products].filter(isRefurbishedGr).map((product) => [product.productId, product])).keys()];
  let step = 1;
  let selectedIds = snapshot.setupCompleted
    ? [...existingProductIds]
    : [...defaultProductIds];
  let loginStart = snapshot.platform?.loginStartEnabled ?? false;
  let autoStartMonitoring = snapshot.config?.autoStartMonitoring ?? true;
  let saving = false;
  let error = "";
  let heading: HTMLHeadingElement;

  $: products = [...new Map([...snapshot.catalog, ...snapshot.products].map((product) => [product.productId, product])).values()];
  $: selectableProducts = products.filter((product) => isRefurbishedGr(product) || product.productId === "108"
    || existingProductIds.includes(product.productId)).sort((a, b) => Number(a.productId) - Number(b.productId));
  $: notificationReady = (snapshot.systemNotificationsEnabled && snapshot.platform?.notificationPermission === "granted")
    || snapshot.channels.some((channel) => channel.enabled && channel.subscriptions.includes("stock_available"));

  function toggleProduct(id: string) {
    selectedIds = selectedIds.includes(id) ? selectedIds.filter((selected) => selected !== id) : [...selectedIds, id];
  }

  async function goTo(next: number) {
    step = next;
    error = "";
    await tick();
    heading.focus();
  }

  async function saveProducts() {
    if (!selectedIds.length || busy || saving) return;
    saving = true;
    error = "";
    try {
      if (await onProducts([...selectedIds])) await goTo(2);
      else error = "产品设置未保存，请重试。";
    } catch (cause) {
      error = `产品设置未保存：${cause instanceof Error ? cause.message : String(cause)}。请重试。`;
    } finally {
      saving = false;
    }
  }

  async function skipProducts() {
    selectedIds = snapshot.setupCompleted ? [...existingProductIds] : [...defaultProductIds];
    await saveProducts();
  }

  async function complete() {
    if (busy || saving) return;
    saving = true;
    error = "";
    try {
      await onComplete(loginStart, autoStartMonitoring);
    } catch (cause) {
      error = `设置未完成：${cause instanceof Error ? cause.message : String(cause)}。请重试。`;
    } finally {
      saving = false;
    }
  }
</script>

<section class="onboarding" aria-label="设置引导">
  {#if step === 3}<CompletionConfetti />{/if}
  <div class="step-content" class:completion={step === 3}>
    {#if step === 1}
      <h1 bind:this={heading} tabindex="-1"><span class="step-number">1/3</span> 选择监控商品</h1>
      <fieldset disabled={busy || saving} class="products" aria-label="监控商品">
        {#each selectableProducts as product (product.productId)}
          <ProductChoice {product} selected={selectedIds.includes(product.productId)} pending={busy || saving} onToggle={() => toggleProduct(product.productId)} />
        {/each}
        {#if !selectableProducts.length}<p class="muted">暂无可选商品</p>{/if}
      </fieldset>
    {:else if step === 2}
      <h1 bind:this={heading} tabindex="-1"><span class="step-number">2/3</span> 设置通知</h1>
      <section class="notification-section" aria-labelledby="system-notification-title">
        <div class="section-heading system-heading"><h2 id="system-notification-title">系统通知</h2><span class="muted">{snapshot.systemNotificationsEnabled && snapshot.platform?.notificationPermission === "granted" ? "已启用" : "待启用"}</span>
        <div class="notification-actions">
          {#if !snapshot.systemNotificationsEnabled || snapshot.platform?.notificationPermission !== "granted"}<button disabled={busy} on:click={onSystemEnable}>启用系统通知</button>{/if}
          <button disabled={busy} on:click={onSystemTest}>测试系统通知</button>
        </div>
        </div>
      </section>
      <section class="notification-section" aria-labelledby="channel-title">
        <div class="section-heading"><h2 id="channel-title">通知渠道</h2><ChannelAdd providers={snapshot.providers} {busy} onAdd={onAddChannel} /></div>
        {#each snapshot.channels as channel (channel.id)}
          <div class="channel-row">
            <ProviderIcon providerId={channel.providerId} name={channel.providerName} />
            <div>
              <div class="channel-title"><strong>{channel.name}</strong><ChannelStatus {channel} /></div>
              <div class="channel-recipients">{#each channel.selectedTargets ?? [] as target, index (`${target.kind}:${target.id}`)}<RecipientChip {target} {index} />{/each}</div>
              {#if !channel.enabled && channel.lastTest?.outcome !== "accepted"}<small id={`channel-enable-hint-${channel.id}`}>先测试，再启用。</small>{/if}
            </div>
            <div class="channel-actions">
              {#if channel.connectionStatus === "auth_required"}<button disabled={busy} on:click={() => onChannelEdit(channel)}>重新授权</button>{/if}
              <button disabled={busy} aria-label={`测试${channel.name}`} on:click={() => onChannelTest(channel.id)}>测试</button>
              <button disabled={busy || (!channel.enabled && (channel.lastTest?.outcome !== "accepted" || channel.connectionStatus === "auth_required"))} aria-label={`${channel.enabled ? "停用" : "启用"}${channel.name}`} aria-describedby={!channel.enabled && channel.lastTest?.outcome !== "accepted" ? `channel-enable-hint-${channel.id}` : undefined} on:click={() => onChannelToggle(channel.id, !channel.enabled)}>{channel.enabled ? "停用" : "启用"}</button>
            </div>
          </div>
        {/each}
      </section>
    {:else}
      <h1 bind:this={heading} tabindex="-1">恭喜！</h1>
      <p class="completion-title">现在您拥有了自己的理光商城上架监控。</p>
      <p class="completion-message">{notificationReady ? "商品上架或库存增加时，会立即提醒你。" : "通知尚未启用，可在 App 内查看监控消息。"}</p>
      <div class="startup-options">
        <label class="login-start"><input type="checkbox" bind:checked={loginStart} disabled={busy || saving} /><span>登录后启动</span></label>
        <label class="login-start"><input type="checkbox" bind:checked={autoStartMonitoring} disabled={busy || saving} /><span>启动后自动开启监控</span></label>
      </div>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>

  <footer class="step-actions">
    <div class="back-actions">{#if step === 1}<button class="text-button" disabled={busy || saving} title="取消尚未保存的商品选择" on:click={onCancel}>取消</button>{:else if step === 2}<button class="text-button" disabled={busy || saving} on:click={() => goTo(step - 1)}>上一步</button>{/if}</div>
    <div class="next-actions">
      {#if step === 1}<button class="text-button" disabled={busy || saving} title={snapshot.setupCompleted ? "沿用已有监控商品" : "使用默认监控商品"} on:click={skipProducts}>跳过</button><button class="primary" disabled={!selectedIds.length || busy || saving} on:click={saveProducts}>{saving ? "正在保存…" : "下一步"}</button>
      {:else if step === 2}<button class="text-button" disabled={busy} on:click={() => goTo(3)}>跳过</button><button class="primary" disabled={busy} on:click={() => goTo(3)}>下一步</button>
      {:else}<button class="primary" disabled={busy || saving} on:click={complete}>{saving ? "正在完成…" : "完成"}</button>{/if}
    </div>
  </footer>
</section>

<style>
  .onboarding { position: relative; box-sizing: border-box; width: min(100%, 800px); height: 100%; min-height: 0; flex: 1; margin: 0 auto; padding: 32px 36px 24px; display: flex; flex-direction: column; color: var(--foreground); }
  .section-heading, .step-actions, .next-actions, .notification-actions { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .step-content { flex: 1; min-height: 0; overflow-y: auto; padding: 2px 6px 8px 2px; }
  .step-content.completion { display: flex; flex-direction: column; justify-content: center; align-items: center; text-align: center; }
  .step-actions { flex-shrink: 0; }
  .back-actions { display: flex; gap: 16px; }
  h1 { margin: 0 0 24px; font-size: 27px; font-weight: 600; line-height: 1.3; letter-spacing: -.5px; }
  h1:focus { outline: none; }
  .step-number { color: var(--muted); font-size: 15px; font-weight: 500; vertical-align: middle; margin-right: 12px; }
  h2 { margin: 0; font-size: 16px; font-weight: 600; }
  .muted, small { color: var(--muted); font-size: 12px; line-height: 1.6; }
  .products { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; min-width: 0; padding: 0; margin: 0; border: 0; }
  input[type="checkbox"] { flex: none; width: 16px; height: 16px; margin: 0; accent-color: var(--accent); cursor: pointer; }
  .notification-section { padding: 22px 0; border-top: 1px solid var(--border); }
  .notification-section:first-of-type { padding-top: 0; border-top: 0; }
  .system-heading { justify-content: flex-start; }
  .notification-actions { justify-content: flex-end; gap: 10px; margin-left: auto; }
  .channel-row { display: flex; justify-content: space-between; align-items: center; gap: 20px; padding: 14px 0; border-bottom: 1px solid var(--hairline); }
  .channel-actions { display: flex; flex: none; gap: 8px; }
  .channel-row strong { font-size: 13px; font-weight: 550; }
  .channel-title { display: flex; align-items: center; gap: 8px; }
  .channel-recipients { display: flex; flex-wrap: wrap; gap: 5px 12px; margin-top: 6px; }
  .channel-row > div:first-of-type { flex: 1; min-width: 0; }
  .channel-row small { display: block; margin-top: 3px; }
  .completion h1 { margin-bottom: 12px; font-size: 36px; }
  .completion-title { margin: 0; font-size: 19px; line-height: 1.7; }
  .completion-message { margin: 12px 0 0; color: var(--muted); font-size: 14px; line-height: 1.7; }
  .startup-options { display: grid; gap: 12px; margin-top: 28px; text-align: left; }
  .login-start { display: flex; align-items: center; gap: 10px; font-size: 14px; cursor: pointer; }
  .error { color: var(--danger); }
  .error { margin: 16px 0 0; font-size: 12px; line-height: 1.6; }
  .step-actions { margin-top: 20px; }
  button { display: inline-flex; align-items: center; justify-content: center; min-height: 36px; padding: 7px 14px; border: 1px solid var(--border); border-radius: 5px; background: var(--button-bg); color: var(--foreground); font: inherit; font-size: 12px; cursor: pointer; transition: background 160ms var(--ease-ui); }
  button:hover { background: var(--button-hover); }
  button:active { transform: translateY(1px); }
  button:disabled { color: var(--disabled-text); cursor: default; transform: none; }
  .primary { min-width: 104px; background: var(--primary-bg); border-color: var(--primary-bg); color: white; font-weight: 550; }
  .primary:hover { background: var(--accent-hover); }
  .primary:disabled { background: var(--primary-disabled-bg); border-color: var(--primary-disabled-border); color: var(--primary-disabled-text); }
  .text-button { padding-inline: 0; border-color: transparent; background: transparent; color: var(--muted); }
  .text-button:hover { background: transparent; color: var(--foreground); }
  button:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  @media (max-width: 760px) { .onboarding { padding: 24px; } h1 { font-size: 24px; } }
  @media (max-width: 520px) { .products { grid-template-columns: 1fr; } .onboarding { padding-inline: 16px; } }
  @media (prefers-reduced-motion: reduce) { button { transition: none; } }
</style>
