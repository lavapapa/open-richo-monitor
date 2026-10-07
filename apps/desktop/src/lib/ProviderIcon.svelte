<script lang="ts">
  export let providerId: string;
  export let name = "";
  export let size = 26;
  // 图标来自各平台官网，保留品牌原始配色。
  // https://www.feishu.cn/ · https://work.weixin.qq.com/ · https://www.dingtalk.com/ · https://weixin.qq.com/
  const icons: Record<string, string> = {
    feishu: "/providers/feishu.png",
    wecom: "/providers/wecom.png",
    dingtalk: "/providers/dingtalk.png",
    weixin: "/providers/weixin.png",
  };
</script>

<span class="provider-icon" style={`width:${size}px;height:${size}px`}>
  {#if icons[providerId]}<img src={icons[providerId]} alt={name} width={size} height={size} />{:else}<span aria-hidden="true">{name.slice(0, 1)}</span>{/if}
</span>

<style>
  .provider-icon { display: inline-grid; place-items: center; flex: none; overflow: hidden; border-radius: 6px; }
  img { display: block; width: 100%; height: 100%; object-fit: contain; }
</style>
