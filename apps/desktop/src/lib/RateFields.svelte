<script lang="ts">
  import type { MonitoringConfig } from "$lib/desktop-api";
  export let rate: MonitoringConfig["rate"];
  export let monitoringMode: MonitoringConfig["monitoringMode"];
  export let target: "draft" | "config";
  export let onChange: (key: keyof MonitoringConfig["rate"], value: number) => void;
  const fields: { key: keyof MonitoringConfig["rate"]; label: string; min: number; step: number }[] = [
    { key: "intervalMinMs", label: "最小检查间隔（秒）", min: 0.1, step: 0.1 },
    { key: "intervalMaxMs", label: "最大检查间隔（秒）", min: 0.1, step: 0.1 },
    { key: "failuresBeforeBackoff", label: "连续失败几次后等待", min: 1, step: 1 },
    { key: "failureBackoffSeconds", label: "连续失败等待（秒）", min: 1, step: 1 },
  ];

  function displayValue(key: keyof MonitoringConfig["rate"], currentRate: MonitoringConfig["rate"]) {
    return key === "intervalMinMs" || key === "intervalMaxMs" ? currentRate[key] / 1000 : currentRate[key];
  }

  function updateRate(key: keyof MonitoringConfig["rate"], input: HTMLInputElement) {
    if (!input.value || !input.validity.valid || !Number.isFinite(input.valueAsNumber)) return;
    onChange(key, input.valueAsNumber * (key === "intervalMinMs" || key === "intervalMaxMs" ? 1000 : 1));
  }

  function restoreInvalid(key: keyof MonitoringConfig["rate"], input: HTMLInputElement) {
    if (!input.value || !input.validity.valid) input.value = String(displayValue(key, rate));
  }
</script>

<p class="rate-hint">{monitoringMode === "listed_products" ? "每轮列表检查间隔" : "每个监控商品请求间隔"}在 {rate.intervalMinMs / 1000}～{rate.intervalMaxMs / 1000} 秒之间随机选取。</p>
<div class="form-grid">
  {#each fields as field}
    <div class="form-row">
      <label for={`${target}-${field.key}`}>{field.label}</label>
      <input id={`${target}-${field.key}`} type="number" min={field.min} step={field.step} value={displayValue(field.key, rate)} on:input={(event) => updateRate(field.key, event.currentTarget)} on:blur={(event) => restoreInvalid(field.key, event.currentTarget)} />
    </div>
  {/each}
</div>

<style>
  .rate-hint { margin: 4px 0 0; color: var(--muted); font-size: 12px; line-height: 1.5; }
  .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; margin-top: 16px; }
  .form-row { display: grid; gap: 7px; min-width: 0; }
  .form-row label { color: var(--foreground); font-size: 12px; font-weight: 600; }
  .form-row input { box-sizing: border-box; width: 100%; min-width: 0; height: 36px; border: 1px solid var(--control-border); border-radius: 5px; background: var(--control); color: var(--foreground); color-scheme: inherit; padding: 0 10px; font: inherit; font-size: 13px; font-variant-numeric: tabular-nums; caret-color: var(--accent); }
  .form-row input:hover { border-color: var(--control-hover); }
  .form-row input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  @media (max-width: 460px) { .form-grid { grid-template-columns: 1fr; } }
</style>
