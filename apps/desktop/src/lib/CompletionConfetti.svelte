<script lang="ts">
  import { onMount } from "svelte";
  import confetti from "canvas-confetti";
  let canvas: HTMLCanvasElement;
  onMount(() => {
    const fire = confetti.create(canvas, { resize: true, disableForReducedMotion: true });
    // 沿用官方 Realistic Look 示例：https://www.kirilv.com/canvas-confetti/#realistic
    const burst = (ratio: number, options: confetti.Options) => fire({ origin: { y: 0.7 }, ...options, particleCount: Math.floor(200 * ratio) });
    burst(0.25, { spread: 26, startVelocity: 55 });
    burst(0.2, { spread: 60 });
    burst(0.35, { spread: 100, decay: 0.91, scalar: 0.8 });
    burst(0.1, { spread: 120, startVelocity: 25, decay: 0.92, scalar: 1.2 });
    burst(0.1, { spread: 120, startVelocity: 45 });
    return () => fire.reset();
  });
</script>

<canvas bind:this={canvas} class="confetti" aria-hidden="true"></canvas>

<style>
  .confetti { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; }
</style>
