<script lang="ts">
  import Power from "@lucide/svelte/icons/power";
  import Pause from "@lucide/svelte/icons/pause";
  import Plug from "@lucide/svelte/icons/plug";
  import Unplug from "@lucide/svelte/icons/unplug";
  import LoaderCircle from "@lucide/svelte/icons/loader-circle";
  import CircleCheck from "@lucide/svelte/icons/circle-check";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import KeyRound from "@lucide/svelte/icons/key-round";
  import CircleDashed from "@lucide/svelte/icons/circle-dashed";
  import type { NotificationChannel } from "$lib/desktop-api";
  export let channel: NotificationChannel;
  export let testing = false;
  export let deliveryDescription = "";
  $: connection = channel.connectionStatus;
  $: connecting = connection === "connecting" || connection === "reconnecting";
  $: connectionText = ({ ready: "已连接", connecting: "连接中", reconnecting: "重新连接中", auth_required: "需要重新授权", failed: "连接失败", stopped: "连接已停止" } as Record<string, string>)[connection ?? "stopped"] ?? "连接未就绪";
  $: testText = testing ? "正在测试" : channel.lastTest?.outcome === "accepted" ? "测试成功" : channel.lastTest?.outcome === "failed" ? "测试失败" : channel.lastTest?.outcome === "unknown" ? "测试结果未知" : "尚未测试";
  $: description = `${channel.enabled ? "已启用" : "已停用"} · ${connectionText} · ${testText}${channel.lastTest?.outcome !== "accepted" && channel.lastTest?.message ? `：${channel.lastTest.message}` : ""}${deliveryDescription ? ` · ${deliveryDescription}` : ""}`;
</script>

<span class="status-tag" title={description} aria-label={description}>
  <span class:good={channel.enabled}>{#if channel.enabled}<Power size={12} aria-hidden="true" />{:else}<Pause size={12} aria-hidden="true" />{/if}</span>
  <span class:good={connection === "ready"} class:bad={connection === "failed" || connection === "auth_required"} class:spinning={connecting}>{#if connecting}<LoaderCircle size={12} aria-hidden="true" />{:else if connection === "ready"}<Plug size={12} aria-hidden="true" />{:else if connection === "auth_required"}<KeyRound size={12} aria-hidden="true" />{:else}<Unplug size={12} aria-hidden="true" />{/if}</span>
  <span class:good={channel.lastTest?.outcome === "accepted" && !testing} class:bad={channel.lastTest?.outcome === "failed" && !testing} class:spinning={testing}>{#if testing}<LoaderCircle size={12} aria-hidden="true" />{:else if channel.lastTest?.outcome === "accepted"}<CircleCheck size={12} aria-hidden="true" />{:else if channel.lastTest}<CircleAlert size={12} aria-hidden="true" />{:else}<CircleDashed size={12} aria-hidden="true" />{/if}</span>
</span>

<style>
  .status-tag { display: inline-flex; flex: none; align-items: center; gap: 6px; height: 22px; padding: 0 7px; border: 1px solid var(--hairline); border-radius: 999px; background: var(--subtle); color: var(--muted); }
  .status-tag > span { display: inline-flex; }
  .good { color: var(--positive); } .bad { color: var(--danger); }
  .spinning { animation: spin 1s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spinning { animation: none; } }
</style>
