<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import QRCode from "qrcode";
  import { desktopApi, type ChannelBinding, type ChannelTarget, type NotificationChannel } from "$lib/desktop-api";
  import ProviderIcon from "$lib/ProviderIcon.svelte";
  import RecipientChip from "$lib/RecipientChip.svelte";
  import Check from "@lucide/svelte/icons/check";
  import Pencil from "@lucide/svelte/icons/pencil";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import Pause from "@lucide/svelte/icons/pause";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";

  export let providerId: string;
  export let providerName: string;
  export let existing: NotificationChannel | null = null;
  export let binding: ChannelBinding | null = null;
  export let targetId = "";
  export let targetKind = "chat";
  export let selectedTargets: ChannelTarget[] = [];
  export let channelName = "";
  export let defaultName = "";
  export let connected = false;
  let editingName = false;
  let nameInput: HTMLInputElement;
  let editingTargetKey = "";

  let qrImage = "";
  let pending = false;
  let verificationCode = "";
  let submittingCode = false;
  let detecting = false;
  let detectedTargets: ChannelTarget[] = [];
  let groupsDetected = false;
  let defaultRecipientSelected = false;
  let error = "";
  let botError = "";
  let groupsError = "";
  let editingChannelId: string | null = null;
  let recipientSettingsOpen = !!existing;
  let generation = 0;
  let sessionId: string | null = null;
  let renderedQrUrl = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  $: availableTargets = binding?.targets ?? existing?.targets ?? [];
  $: targets = mergeTargets([...(groupsDetected && providerId === "feishu" ? availableTargets.filter((target) => target.kind !== "chat") : availableTargets), ...detectedTargets, ...selectedTargets]);
  $: needsAuthentication = existing?.connectionStatus === "auth_required";
  $: connected = binding?.status === "complete" || (!binding && !!existing && !needsAuthentication);
  $: canDetectGroups = ["feishu", "wecom", "dingtalk"].includes(providerId);
  $: connectionStatus = binding ? binding.connectionStatus : existing?.connectionStatus;
  $: ready = connectionStatus === "ready";
  $: busyConnection = connectionStatus === "connecting" || connectionStatus === "reconnecting";
  $: displayedName = channelName || binding?.botName || existing?.botName || defaultName || `${providerName} 1`;
  $: botUrl = binding ? binding.botUrl : existing?.botUrl;
  $: scanHint = providerId === "weixin" ? "用微信扫码，在手机确认后返回"
    : providerId === "feishu" ? "用飞书扫码创建，打开机器人后返回"
    : providerId === "wecom" ? "用企业微信扫码创建，给机器人发私信后返回"
    : "用钉钉扫码创建，打开机器人发私信后返回";
  $: groupHint = providerId === "feishu" ? "把机器人加入群聊，点击刷新群聊"
    : providerId === "wecom" ? "把机器人加入群聊，在群内@它发消息"
    : "把机器人加入内部群，在群内@它发消息";

  function mergeTargets(items: ChannelTarget[]) {
    const merged = new Map<string, ChannelTarget>();
    for (const target of items) {
      const key = `${target.kind}:${target.id}`;
      const old = merged.get(key);
      merged.set(key, old && (!target.label || target.label === target.id) ? { ...target, label: old.label } : target);
    }
    return [...merged.values()];
  }

  function targetLabel(target: ChannelTarget, index: number) {
    return target.label && (target.kind === "user" || target.label !== target.id) ? target.label : target.kind === "user" ? `个人 ${index + 1}` : "群聊";
  }

  function renameTarget(target: ChannelTarget, label: string) {
    const updated = { ...target, label: label.trim() || target.label };
    detectedTargets = mergeTargets([...detectedTargets, updated]);
    selectedTargets = selectedTargets.map((item) => item.id === target.id && item.kind === target.kind ? updated : item);
  }

  async function openBot() {
    if (!botUrl) return;
    botError = "";
    try { await desktopApi.openExternalUrl(botUrl); }
    catch (cause) { botError = cause instanceof Error ? cause.message : String(cause); }
  }

  function stop() {
    generation += 1;
    clearTimeout(timer);
    detecting = false;
    const id = sessionId;
    sessionId = null;
    binding = null;
    botError = "";
    if (id) void desktopApi.cancelChannelBinding(id).catch(() => {});
    if (editingChannelId) {
      const editingId = editingChannelId;
      editingChannelId = null;
      void desktopApi.endChannelEditing(editingId).catch(() => {});
    }
  }

  async function apply(next: ChannelBinding, request: number) {
    if (request !== generation) return;
    binding = next;
    sessionId = next.id;
    if (!existing && !defaultRecipientSelected && next.status === "complete") {
      const recipient = next.targets.find((target) => target.kind === "user");
      if (recipient && !selectedTargets.length) selectedTargets = [{ ...recipient }];
      if (recipient || selectedTargets.length) defaultRecipientSelected = true;
    }
    if (next.qrUrl && next.qrUrl !== renderedQrUrl && next.status !== "complete") {
      const image = await QRCode.toDataURL(next.qrUrl, { width: 232, margin: 2, errorCorrectionLevel: "M" });
      if (request === generation) { qrImage = image; renderedQrUrl = next.qrUrl; }
    }
    if (["waiting", "scanned", "needs_verification", "complete"].includes(next.status) && request === generation) {
      timer = setTimeout(() => poll(next.id, request), 2000);
    }
  }

  async function poll(id: string, request: number) {
    try {
      await apply(await desktopApi.channelBindingStatus(id), request);
    } catch (cause) {
      if (request === generation) error = `连接状态获取失败：${cause instanceof Error ? cause.message : String(cause)}。请重新扫码。`;
    }
  }

  async function start() {
    stop();
    verificationCode = "";
    submittingCode = false;
    selectedTargets = [];
    detectedTargets = [];
    groupsDetected = false;
    groupsError = "";
    defaultRecipientSelected = false;
    targetId = "";
    targetKind = providerId === "weixin" ? "user" : "chat";
    const request = generation;
    pending = true;
    error = "";
    qrImage = "";
    renderedQrUrl = "";
    try {
      const next = existing ? await desktopApi.beginChannelRebinding(existing.id) : await desktopApi.beginChannelBinding(providerId);
      if (request !== generation) {
        await desktopApi.cancelChannelBinding(next.id);
        return;
      }
      await apply(next, request);
    } catch (cause) {
      if (request === generation) error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (request === generation) pending = false;
    }
  }

  async function submitVerification() {
    if (!sessionId || submittingCode) return;
    const id = sessionId;
    // 提交结果取代本阶段轮询；已发出的旧状态响应不得回退界面。
    const request = ++generation;
    clearTimeout(timer);
    submittingCode = true;
    error = "";
    try {
      const next = await desktopApi.submitChannelBindingVerification(id, verificationCode.trim());
      if (request !== generation) return;
      clearTimeout(timer);
      verificationCode = "";
      await apply(next, request);
    } catch (cause) {
      if (request === generation) {
        error = cause instanceof Error ? cause.message : String(cause);
        timer = setTimeout(() => poll(id, request), 2000);
      }
    } finally { if (request === generation) submittingCode = false; }
  }

  function isSelected(target: ChannelTarget) {
    return selectedTargets.some((selected) => selected.id === target.id && selected.kind === target.kind);
  }

  function toggleTarget(target: ChannelTarget) {
    selectedTargets = isSelected(target) ? selectedTargets.filter((selected) => selected.id !== target.id || selected.kind !== target.kind) : [...selectedTargets, { ...target }];
  }

  function addManualTarget() {
    const id = targetId.trim();
    if (!id) return;
    const target = { id, kind: providerId === "weixin" ? "user" : targetKind, label: id };
    detectedTargets = [...detectedTargets, target];
    if (!isSelected(target)) selectedTargets = [...selectedTargets, target];
    targetId = "";
  }

  async function detectGroups() {
    if (detecting) return;
    const request = generation;
    const bindingId = sessionId;
    detecting = true;
    groupsError = "";
    try {
      const groups = bindingId ? await desktopApi.detectBindingGroups(bindingId) : await desktopApi.detectChannelGroups(existing!.id);
      if (request === generation) { detectedTargets = groups.map((target) => ({ ...target })); groupsDetected = true; }
    } catch (cause) {
      if (request === generation) groupsError = `读取失败：${cause instanceof Error ? cause.message : String(cause)}`;
    } finally {
      if (request === generation) detecting = false;
    }
  }

  onMount(() => {
    if (!existing || needsAuthentication) { void start(); return; }
    const id = existing.id;
    const request = generation;
    void desktopApi.beginChannelEditing(id).then(() => {
      if (request !== generation) void desktopApi.endChannelEditing(id).catch(() => {});
      else editingChannelId = id;
    }).catch((cause) => {
      if (request === generation) groupsError = cause instanceof Error ? cause.message : String(cause);
    });
  });
  onDestroy(stop);
</script>

<section class="binding" aria-label={`${providerName}扫码连接`}>
  {#if pending || (binding?.status === "waiting" && !qrImage)}<div class="qr-stage" role="status" aria-label="正在加载二维码"><span class="spinner" aria-hidden="true"></span></div>
  {:else if connected}
    <div class="connected-state">
      <div class="connected-icon" role="img" aria-label={ready ? `已连接${providerName}` : busyConnection ? `${providerName}连接中` : `${providerName}连接未就绪`}><ProviderIcon {providerId} name="" size={64} /><span class="connection-mark" class:ready class:connecting={busyConnection}>{#if ready}<Check size={17} />{:else if busyConnection}<LoaderCircle size={17} />{:else if connectionStatus === "failed"}<CircleAlert size={17} />{:else}<Pause size={17} />{/if}</span></div>
      <div class="name-row"><input bind:this={nameInput} class="channel-name" aria-label="渠道名称" value={displayedName} placeholder={defaultName} readonly={!editingName} on:input={(event) => channelName = event.currentTarget.value} on:blur={() => editingName = false} /><button class="binding-retry" aria-label="编辑渠道名称" title="编辑名称" on:click={() => { editingName = true; nameInput.focus(); nameInput.select(); }}><Pencil size={13} /></button></div>
      {#if connectionStatus === "connection_conflict"}<p class="binding-message" role="alert">连接被其他实例接管。请回到通知页，停用后重新启用。</p>{/if}
      {#if botUrl}<button class="binding-retry" on:click={openBot}>{providerId === "dingtalk" ? "查看钉钉机器人" : "打开机器人"}</button>{/if}
      {#if botError}<p role="alert">{botError}</p>{/if}
      {#if selectedTargets.length}<div class="connected-recipients" aria-label="已选接收对象">{#each selectedTargets as target, index (`${target.kind}:${target.id}`)}<RecipientChip {target} {index} />{/each}</div>{/if}
      {#if !selectedTargets.length}<p role="status">{!ready ? busyConnection ? `账户已授权，正在连接${providerName}。` : `账户已授权，${providerName}连接未就绪。请检查网络后重试。` : binding?.message || (targets.length ? "请选择通知接收位置。" : "平台尚未返回接收对象。可使用手动配置添加。")}</p>{/if}
    </div>
    <details class="recipient-settings" bind:open={recipientSettingsOpen}><summary>接收位置</summary>
      {#if canDetectGroups}<div class="group-tools"><span>{providerId === "feishu" ? "机器人所在群聊" : "已识别的群聊"}</span><button class="binding-retry" disabled={detecting} on:click={detectGroups}>{detecting ? "正在刷新…" : "刷新群聊"}</button></div>{/if}
      {#if canDetectGroups}<p class="binding-message">{groupHint}</p>{/if}
      {#if groupsError}<p role="alert">{groupsError}</p>{/if}
      {#if targets.length}
      <fieldset class="binding-targets" aria-label="通知接收对象">{#each targets as target, index (`${target.kind}:${target.id}`)}<div class="target-row"><label><input aria-label={`${targetLabel(target, index)} ${target.kind === "user" ? "个人" : "群聊"}`} type="checkbox" checked={isSelected(target)} on:change={() => toggleTarget(target)} /><RecipientChip {target} {index} /></label><button class="binding-retry" aria-label={`命名${targetLabel(target, index)}`} title="修改显示名称" on:click={() => editingTargetKey = editingTargetKey === `${target.kind}:${target.id}` ? "" : `${target.kind}:${target.id}`}><Pencil size={12} /></button>{#if editingTargetKey === `${target.kind}:${target.id}`}<input class="target-name" aria-label="接收对象名称" value={targetLabel(target, index)} on:change={(event) => renameTarget(target, event.currentTarget.value)} />{/if}</div>{/each}</fieldset>
      {:else}<p class="binding-message">暂未识别到接收对象。</p>{/if}
    <details><summary>手动填写接收对象</summary><label class="binding-field">接收对象 ID<input aria-label="接收对象 ID" bind:value={targetId} /></label>{#if providerId !== "weixin"}<label class="binding-field">对象类型<select aria-label="对象类型" bind:value={targetKind}><option value="chat">群聊</option><option value="user">个人</option></select></label>{/if}<button class="binding-retry" disabled={!targetId.trim()} on:click={addManualTarget}>添加接收对象</button></details>
    <div class="reconnect-tools"><button class="binding-retry" on:click={start}>重新扫码</button></div>
    </details>
  {:else if binding?.status === "needs_verification"}
    <form class="qr-stage" on:submit|preventDefault={submitVerification}>
      <p class="binding-message">输入手机配对码，确认后继续</p>
      <label class="binding-field">手机配对码<input aria-label="手机配对码" inputmode="numeric" autocomplete="one-time-code" maxlength="6" bind:value={verificationCode} /></label>
      <button class="binding-retry" type="submit" disabled={submittingCode || !/^\d{6}$/.test(verificationCode.trim())}>{submittingCode ? "正在验证…" : "确认配对"}</button>
    </form>
  {:else if binding?.status === "waiting" || binding?.status === "scanned"}
    <div class="qr-stage">{#if qrImage}<img class="binding-qr" src={qrImage} alt={`使用${providerName}扫描二维码`} width="232" height="232" />{/if}
    <p class="binding-message scan-hint" role="status">{scanHint}</p></div>
  {:else if binding?.status === "expired"}
    <div class="qr-stage"><button class="qr-refresh" aria-label="刷新二维码" title="二维码已过期，刷新" on:click={start}><RefreshCw size={25} aria-hidden="true" /></button></div>
  {:else if binding?.status === "failed" || binding?.status === "cancelled"}
    <p role="status">连接未完成，请重新扫码。</p><button class="binding-retry" on:click={start}>重新获取二维码</button>
  {/if}
  {#if binding?.message && binding.status === "failed"}<p role="alert">{binding.message}</p>{/if}
  {#if error}<p role="alert">{error}</p><button class="binding-retry" disabled={pending} on:click={start}>重新获取二维码</button>{/if}
</section>

<style>
  .binding { margin: 0; }
  .binding p { margin: 10px 0; font-size: 13px; line-height: 1.5; }
  .qr-stage { min-height: 310px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; text-align: center; }
  .qr-refresh { display: grid; place-items: center; width: 58px; height: 58px; border: 0; border-radius: 50%; color: var(--muted); background: var(--subtle); cursor: pointer; }
  .qr-refresh:hover { color: var(--foreground); background: var(--button-hover); }
  .spinner { width: 28px; height: 28px; border: 3px solid var(--border); border-top-color: var(--foreground); border-radius: 50%; animation: spin 900ms linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spinner { animation: none; } }
  .connected-state { padding: 28px 0; text-align: center; overflow-wrap: anywhere; }
  .connected-icon { display: inline-flex; position: relative; margin-bottom: 16px; }
  .connection-mark { display: grid; place-items: center; position: absolute; right: -5px; bottom: -5px; width: 25px; height: 25px; border: 3px solid var(--surface); border-radius: 50%; background: var(--subtle); color: var(--muted); }
  .connection-mark.ready { background: var(--positive); color: var(--surface); }
  .connection-mark.connecting :global(svg) { animation: spin 1s linear infinite; }
  .name-row { display: flex; justify-content: center; align-items: center; gap: 6px; }
  .name-row .channel-name { width: min(280px, 80%); font-size: 17px; font-weight: 600; text-align: center; }
  .name-row .channel-name[readonly] { border-color: transparent; background: transparent; }
  .connected-recipients { display: flex; justify-content: center; flex-wrap: wrap; gap: 8px 14px; margin-top: 12px; }
  .recipient-settings { padding: 12px 0; }
  .group-tools { display: flex; justify-content: space-between; align-items: center; gap: 12px; margin: 16px 0 10px; }
  .reconnect-tools { margin-top: 16px; text-align: right; }
  .binding-qr { display: block; max-width: 100%; height: auto; margin: 0 auto; border-radius: 5px; }
  .binding-retry { padding: 4px 0; border: 0; background: transparent; color: var(--muted); cursor: pointer; font: inherit; font-size: 12px; }
  .binding-retry:hover { color: var(--foreground); }
  .binding-field { display: grid; gap: 6px; margin: 12px 0; font-size: 13px; }
  .binding-targets { display: grid; gap: 10px; margin: 12px 0; padding: 0; border: 0; min-width: 0; font-size: 13px; }
  .binding-targets label { display: flex; align-items: center; gap: 8px; cursor: pointer; }
  .target-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .target-row label { flex: 1; min-width: 0; }
  .target-row .target-name { flex: 1 0 100%; }
  input:not([type="checkbox"]), select { width: 100%; min-height: 34px; box-sizing: border-box; padding: 6px 9px; color: var(--foreground); background: var(--surface); border: 1px solid var(--control-border); border-radius: 5px; font: inherit; }
  details { font-size: 12px; color: var(--muted); } summary { cursor: pointer; }
  .binding-message { color: var(--muted); }
  .scan-hint { white-space: nowrap; }
  [role="alert"] { color: var(--danger); }
  button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
</style>
