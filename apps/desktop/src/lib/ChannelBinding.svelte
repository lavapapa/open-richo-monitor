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
  export let rebindOnMount = false;
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
  let verificationError = "";
  let submittingCode = false;
  let detecting = false;
  let detectedTargets: ChannelTarget[] = [];
  let groupsDetected = false;
  let defaultRecipientSelected = false;
  let error = "";
  let botError = "";
  let groupsError = "";
  let editingChannelId: string | null = null;
  let generation = 0;
  let sessionId: string | null = null;
  let renderedQrUrl = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  $: availableTargets = binding?.targets ?? existing?.targets ?? [];
  $: targets = mergeTargets([...(groupsDetected && providerId === "feishu" ? availableTargets.filter((target) => target.kind !== "chat") : availableTargets), ...detectedTargets, ...selectedTargets]);
  $: personalTargets = targets.filter((target) => target.kind === "user").slice(0, providerId === "weixin" ? 1 : undefined);
  $: groupTargets = targets.filter((target) => target.kind === "chat");
  $: needsAuthentication = existing?.connectionStatus === "auth_required";
  $: connected = binding?.status === "complete" || (!binding && !!existing && !needsAuthentication && !rebindOnMount);
  $: canDetectGroups = ["feishu", "wecom", "dingtalk"].includes(providerId);
  $: connectionStatus = binding ? binding.connectionStatus : existing?.connectionStatus;
  $: ready = connectionStatus === "ready";
  $: busyConnection = connectionStatus === "connecting" || connectionStatus === "reconnecting";
  $: displayedName = channelName || binding?.botName || existing?.botName || defaultName || `${providerName} 1`;
  $: botUrl = binding ? binding.botUrl : existing?.botUrl;
  $: scanHint = providerId === "weixin" ? "用微信app扫码，连接并发送任意私信后返回"
    : providerId === "feishu" ? "用飞书app扫码，创建机器人并打开应用后返回"
    : providerId === "wecom" ? "用企业微信app扫码，创建机器人并发送任意私信后返回"
    : "用钉钉app扫码，创建机器人并发送任意私信后返回";
  $: groupHint = providerId === "feishu" ? "把机器人加入群聊，点击刷新群聊"
    : providerId === "wecom" ? "加入群聊后，在群内@它发任意消息"
    : "加入内部群后，在群内@它发任意消息";
  $: personalReply = binding?.pairingReplies?.find(reply => reply.target.kind === "user" && selectedTargets.some(target => target.kind === "user" && target.id === reply.target.id));
  $: groupOnly = selectedTargets.length > 0 && selectedTargets.every(target => target.kind === "chat");
  $: pairingHint = !ready ? busyConnection ? `账户已授权，正在连接${providerName}。` : `账户已授权，${providerName}连接未就绪。请检查网络后重试。`
    : groupOnly ? "群聊已选择，可以保存"
    : personalReply?.outcome === "sending" ? "已收到私信，正在发送确认"
    : personalReply && ["failed", "unknown"].includes(personalReply.outcome) ? "已收到私信，确认消息发送未成功"
    : binding?.privateMessageReceived ? "已收到私信，配对成功"
    : providerId === "feishu" ? binding?.privateChatReady ? "配对成功" : "请打开机器人应用，再返回保存"
    : "请向机器人发送任意私信，再返回保存";

  function groupReply(target: ChannelTarget) {
    return binding?.pairingReplies?.find(reply => reply.target.id === target.id && reply.target.kind === target.kind);
  }

  export function restart() { return start(); }

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
    if (target.kind === "user" && target.label === "绑定账号") return "创建人";
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
    error = "";
    if (next.status !== "needs_verification") verificationError = "";
    binding = next;
    sessionId = next.id;
    if (providerId === "weixin" && next.status === "complete") {
      const creator = next.targets.find(target => target.kind === "user");
      selectedTargets = creator ? [{ ...creator }] : [];
    }
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
      if (request === generation) {
        error = `连接状态获取失败：${cause instanceof Error ? cause.message : String(cause)}`;
        timer = setTimeout(() => poll(id, request), 2000);
      }
    }
  }

  async function start() {
    stop();
    verificationCode = "";
    verificationError = "";
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
    verificationError = "";
    try {
      const next = await desktopApi.submitChannelBindingVerification(id, verificationCode.trim());
      if (request !== generation) return;
      clearTimeout(timer);
      verificationCode = "";
      await apply(next, request);
    } catch (cause) {
      if (request === generation) {
        verificationError = cause instanceof Error ? cause.message : String(cause);
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
    if (providerId === "weixin" && existing) {
      const creator = existing.selectedTargets?.find(target => target.kind === "user") ?? existing.targets?.find(target => target.kind === "user");
      selectedTargets = creator ? [{ ...creator }] : [];
    }
    if (!existing || needsAuthentication || rebindOnMount) { void start(); return; }
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
    <div aria-label="通知接收对象">
    <div class="connected-state">
      <div class="connected-icon" role="img" aria-label={ready ? `已连接${providerName}` : busyConnection ? `${providerName}连接中` : `${providerName}连接未就绪`}><ProviderIcon {providerId} name="" size={64} /><span class="connection-mark" class:ready class:connecting={busyConnection}>{#if ready}<Check size={17} />{:else if busyConnection}<LoaderCircle size={17} />{:else if connectionStatus === "failed"}<CircleAlert size={17} />{:else}<Pause size={17} />{/if}</span></div>
      <div class="name-row"><div class="name-editor"><span class="name-width" aria-hidden="true">{displayedName}</span><input bind:this={nameInput} class="channel-name" aria-label="渠道名称" value={displayedName} placeholder={defaultName} readonly={!editingName} on:input={(event) => channelName = event.currentTarget.value} on:blur={() => editingName = false} /></div><button class="binding-retry" aria-label="编辑渠道名称" title="编辑名称" on:click={() => { editingName = true; nameInput.focus(); nameInput.select(); }}><Pencil size={13} /></button></div>
      {#if connectionStatus === "connection_conflict"}<p class="binding-message" role="alert">连接被其他实例接管。请回到通知页，停用后重新启用。</p>{/if}
      {#if botUrl}<button hidden class="binding-retry" on:click={openBot}>{providerId === "dingtalk" ? "查看钉钉机器人" : "打开机器人"}</button>{/if}
      {#if botError}<p role="alert">{botError}</p>{/if}
      {#if personalTargets.length}
        <fieldset class="binding-targets personal-targets" aria-label="私聊接收对象">{#each personalTargets as target, index (`${target.kind}:${target.id}`)}<div class="target-row">{#if providerId === "weixin"}<RecipientChip {target} {index} />{:else}<label><input aria-label={`${targetLabel(target, index)} 个人`} type="checkbox" checked={isSelected(target)} on:change={() => toggleTarget(target)} /><RecipientChip {target} {index} /></label>{/if}</div>{/each}</fieldset>
      {/if}
      {#if binding?.status === "complete"}<p class="binding-message" role="status">{binding.message || pairingHint}</p>{/if}
      {#if !selectedTargets.length && !binding}<p role="status">{!ready ? busyConnection ? `账户已授权，正在连接${providerName}。` : `账户已授权，${providerName}连接未就绪。请检查网络后重试。` : targets.length ? "请勾选通知接收对象。" : "请向机器人发送任意私信，再返回保存"}</p>{/if}
    </div>
    {#if canDetectGroups}
    <section class="group-settings" aria-label="群聊发送">
      <div class="group-tools"><h3>在群聊中发送<span>（可选）</span></h3><button class="binding-retry" disabled={detecting} on:click={detectGroups}>{detecting ? "正在刷新…" : "刷新群聊"}</button></div>
      {#if groupsError}<p role="alert">{groupsError}</p>{/if}
      {#if groupTargets.length}
      <fieldset class="binding-targets" aria-label="群聊接收对象">{#each groupTargets as target, index (`${target.kind}:${target.id}`)}
        {@const reply = groupReply(target)}
        <div class="target-row"><label><input aria-label={`${targetLabel(target, index)} 群聊`} type="checkbox" checked={isSelected(target)} on:change={() => toggleTarget(target)} /><RecipientChip {target} {index} /></label>
        {#if reply}<span class="pairing-mark" class:success={reply.outcome === "accepted"} role="img" aria-label={reply.outcome === "accepted" ? "群聊配对成功" : reply.outcome === "sending" ? "正在发送配对确认" : "配对确认发送未成功"} title={reply.message || "正在发送配对确认"}>{#if reply.outcome === "accepted"}<Check size={14} />{:else if reply.outcome === "sending"}<LoaderCircle size={14} />{:else}<CircleAlert size={14} />{/if}</span>{/if}
        <button class="binding-retry" aria-label={`命名${targetLabel(target, index)}`} title="修改显示名称" on:click={() => editingTargetKey = editingTargetKey === `${target.kind}:${target.id}` ? "" : `${target.kind}:${target.id}`}><Pencil size={12} /></button>{#if editingTargetKey === `${target.kind}:${target.id}`}<input class="target-name" aria-label="接收对象名称" value={targetLabel(target, index)} on:change={(event) => renameTarget(target, event.currentTarget.value)} />{/if}</div>
      {/each}</fieldset>
      {/if}
      <p class="binding-message group-hint">{groupHint}</p>
    </section>
    {/if}
    {#if providerId !== "weixin"}<details><summary>手动填写接收对象</summary><label class="binding-field">接收对象 ID<input aria-label="接收对象 ID" bind:value={targetId} /></label><label class="binding-field">对象类型<select aria-label="对象类型" bind:value={targetKind}><option value="chat">群聊</option><option value="user">个人</option></select></label><button class="binding-retry" disabled={!targetId.trim()} on:click={addManualTarget}>添加接收对象</button></details>{/if}
    </div>
  {:else if binding?.status === "needs_verification"}
    <form class="qr-stage" on:submit|preventDefault={submitVerification}>
      <p class="binding-message">输入手机配对码，确认后继续</p>
      <label class="binding-field">手机配对码<input aria-label="手机配对码" inputmode="numeric" autocomplete="one-time-code" maxlength="6" bind:value={verificationCode} /></label>
      {#if verificationError}<p role="alert">{verificationError}</p>{/if}
      <button class="binding-retry" type="submit" disabled={submittingCode || !/^\d{6}$/.test(verificationCode.trim())}>{submittingCode ? "正在验证…" : "确认配对"}</button>
    </form>
  {:else if binding?.status === "waiting" || binding?.status === "scanned"}
    <div class="qr-stage">{#if qrImage}<img class="binding-qr" src={qrImage} alt={`使用${providerName}扫描二维码`} width="232" height="232" />{/if}
    <div class="scan-tools"><p class="binding-message scan-hint" role="status">{scanHint}</p><button class="binding-retry" aria-label="刷新二维码" on:click={start}>刷新</button></div></div>
  {:else if binding?.status === "expired"}
    <div class="qr-stage"><button class="qr-refresh" aria-label="刷新二维码" title="二维码已过期，刷新" on:click={start}><RefreshCw size={25} aria-hidden="true" /></button></div>
  {:else if binding?.status === "failed" || binding?.status === "cancelled"}
    <p role="status">连接未完成，请重新扫码。</p><button class="binding-retry" on:click={start}>重新获取二维码</button>
  {/if}
  {#if binding?.message && binding.status === "failed"}<p role="alert">{binding.message}</p>{/if}
  {#if error}<p role="alert">{error}</p>{#if !binding}<button class="binding-retry" disabled={pending} on:click={start}>重新获取二维码</button>{/if}{/if}
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
  .connected-state { padding: 18px 0; text-align: center; overflow-wrap: anywhere; }
  .connected-icon { display: inline-flex; position: relative; margin-bottom: 16px; }
  .connection-mark { display: grid; place-items: center; position: absolute; right: -5px; bottom: -5px; width: 25px; height: 25px; border: 3px solid var(--surface); border-radius: 50%; background: var(--subtle); color: var(--muted); }
  .connection-mark.ready { background: var(--positive); color: var(--surface); }
  .connection-mark.connecting :global(svg) { animation: spin 1s linear infinite; }
  .name-row { display: flex; justify-content: center; align-items: center; gap: 6px; }
  .name-editor { position: relative; min-width: 2ch; max-width: calc(100% - 25px); font-size: 17px; font-weight: 600; }
  .name-width { display: block; visibility: hidden; padding: 6px 9px; border: 1px solid transparent; white-space: pre; overflow: hidden; }
  .name-row .channel-name { position: absolute; inset: 0; width: 100%; min-width: 0; font-size: inherit; font-weight: inherit; text-align: center; }
  .name-row .channel-name[readonly] { border-color: transparent; background: transparent; }
  .group-settings { margin: 0 0 18px; }
  .group-tools h3 { margin: 0; font-size: 13px; font-weight: 600; }
  .group-tools h3 span { font-weight: 400; color: var(--muted); }
  .group-hint { text-align: center; }
  .group-tools { display: flex; justify-content: space-between; align-items: center; gap: 12px; margin: 16px 0 10px; }
  .pairing-mark { display: inline-flex; color: var(--muted); }
  .pairing-mark.success { color: var(--positive); }
  .binding-qr { display: block; max-width: 100%; height: auto; margin: 0 auto; border-radius: 5px; }
  .binding-retry { padding: 4px 0; border: 0; background: transparent; color: var(--muted); cursor: pointer; font: inherit; font-size: 12px; }
  .binding-retry:hover { color: var(--foreground); }
  .binding-field { display: grid; gap: 6px; margin: 12px 0; font-size: 13px; }
  .binding-targets { display: grid; gap: 10px; margin: 12px 0; padding: 0; border: 0; min-width: 0; font-size: 13px; }
  .personal-targets { width: fit-content; max-width: 100%; margin: 16px auto 0; }
  .binding-targets label { display: flex; align-items: center; gap: 8px; cursor: pointer; }
  .target-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .target-row label { flex: 1; min-width: 0; }
  .target-row .target-name { flex: 1 0 100%; }
  input:not([type="checkbox"]), select { width: 100%; min-height: 34px; box-sizing: border-box; padding: 6px 9px; color: var(--foreground); background: var(--surface); border: 1px solid var(--control-border); border-radius: 5px; font: inherit; }
  details { font-size: 12px; color: var(--muted); } summary { cursor: pointer; }
  .binding-message { color: var(--muted); }
  .scan-tools { display: flex; align-items: baseline; justify-content: center; gap: 10px; max-width: 100%; }
  .scan-tools p, .scan-tools button { font-size: 12px; }
  .scan-tools button { flex: none; }
  .scan-hint { white-space: nowrap; }
  [role="alert"] { color: var(--danger); }
  button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
</style>
