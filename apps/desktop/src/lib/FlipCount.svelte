<script lang="ts">
  import { onMount } from "svelte";
  import { isTauri } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { createRollingNumber, type RollingNumberController } from "@kitlangton/rolling-number";
  import "@kitlangton/rolling-number/styles.css";

  export let count: number;

  let host: HTMLSpanElement;
  let counter: RollingNumberController | undefined;
  let displayed = count;
  let active = false;

  function showLatest(value: number) {
    if (counter && active && value !== displayed) {
      displayed = value;
      counter.update({ value });
    }
  }
  $: showLatest(count);

  onMount(() => {
    counter = createRollingNumber(host, {
      value: displayed, mode: "flap", locales: "zh-CN", format: { useGrouping: true },
      flipDuration: 220, pauseOffscreen: false,
    });
    let disposed = false;
    let focused = document.hasFocus();
    let nativeWindow: ReturnType<typeof getCurrentWindow> | undefined;
    let nativeVisible = true;
    let unlisten: (() => void) | undefined;
    let focusRevision = 0;
    let resumeFrame = 0;

    function sync() {
      if (!focused || document.hidden || !nativeVisible) {
        active = false;
        cancelAnimationFrame(resumeFrame);
        resumeFrame = 0;
        host.getAnimations?.({ subtree: true }).forEach((animation) => animation.pause());
      } else if (active) showLatest(count);
      else if (!resumeFrame) {
        // 库在隐藏时撤下动画；先恢复最后展示值的卡片，再提交最新计数。
        counter?.refresh();
        resumeFrame = requestAnimationFrame(() => {
          resumeFrame = 0;
          active = true;
          showLatest(count);
        });
      }
    }
    function blur() {
      focusRevision++;
      focused = false;
      sync();
    }
    async function focus() {
      const revision = ++focusRevision;
      if (nativeWindow) {
        const [visible, hasFocus, minimized] = await Promise.all([
          nativeWindow.isVisible(), nativeWindow.isFocused(), nativeWindow.isMinimized(),
        ]);
        if (disposed || revision !== focusRevision) return;
        nativeVisible = visible && !minimized;
        focused = hasFocus;
      } else focused = true;
      if (!disposed) sync();
    }
    function visibility() {
      if (!document.hidden && focused) void focus();
      else sync();
    }

    window.addEventListener("blur", blur);
    window.addEventListener("focus", focus);
    document.addEventListener("visibilitychange", visibility);
    sync();

    if (isTauri()) {
      focused = false;
      sync();
      void (async () => {
        nativeWindow = getCurrentWindow();
        const stop = await nativeWindow.onFocusChanged(({ payload }) => {
          if (payload) void focus();
          else blur();
        });
        if (disposed) { stop(); return; }
        unlisten = stop;
        await focus();
      })();
    }

    return () => {
      disposed = true;
      active = false;
      focusRevision++;
      cancelAnimationFrame(resumeFrame);
      window.removeEventListener("blur", blur);
      window.removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", visibility);
      unlisten?.();
      counter?.destroy();
      counter = undefined;
    };
  });
</script>

<span bind:this={host} class="flip-count" aria-hidden="true">{displayed.toLocaleString("zh-CN")}</span>

<style>
  .flip-count { font: inherit; line-height: 1.4; font-variant-numeric: tabular-nums; --rn-flap-background: var(--canvas); --rn-crease: .5px; }
</style>
