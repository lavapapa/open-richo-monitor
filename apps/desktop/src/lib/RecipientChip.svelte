<script lang="ts">
  import Users from "@lucide/svelte/icons/users";
  import UserRound from "@lucide/svelte/icons/user-round";
  import CircleAlert from "@lucide/svelte/icons/circle-alert";
  import type { ChannelTarget, ChannelDelivery } from "$lib/desktop-api";
  export let target: ChannelTarget;
  export let index = 0;
  export let delivery: ChannelDelivery | null = null;
  $: label = target.label && (target.kind === "user" || target.label !== target.id) ? target.label : target.kind === "user" ? `个人 ${index + 1}` : "群聊";
  $: description = `${target.kind === "user" ? "私聊" : "群聊"}：${label}${delivery ? ` · ${delivery.outcome === "accepted" ? "已发送" : delivery.outcome === "failed" ? "投递失败" : "结果未知"}${delivery.message ? ` · ${delivery.message}` : ""}` : ""}`;
</script>

<span class="recipient" title={description} aria-label={description}>
  {#if target.kind === "user"}<span class="avatar" aria-hidden="true"><UserRound size={12} /></span>{:else}<Users size={14} aria-hidden="true" />{/if}
  <span class="recipient-name">{label}</span>
  {#if delivery && delivery.outcome !== "accepted"}<CircleAlert size={12} class="delivery-warning" aria-hidden="true" />{/if}
</span>

<style>
  .recipient { display: inline-flex; min-width: 0; max-width: 100%; align-items: center; gap: 5px; color: var(--muted); font-size: 12px; }
  .avatar { display: grid; place-items: center; flex: none; width: 19px; height: 19px; border-radius: 50%; background: var(--subtle); border: 1px solid var(--hairline); }
  .recipient-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .recipient :global(.delivery-warning) { flex: none; color: var(--danger); }
</style>
