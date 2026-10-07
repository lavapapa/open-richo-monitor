<script lang="ts">
  import type { ScheduleConfig, Weekday } from "$lib/desktop-api";
  export let schedule: ScheduleConfig;
  export let target: "draft" | "config";
  export let onTime: (key: "start" | "end", value: string) => void;
  export let onDay: (day: Weekday) => void;
  const dayOptions: { id: Weekday; label: string }[] = [{ id: "mon", label: "一" }, { id: "tue", label: "二" }, { id: "wed", label: "三" }, { id: "thu", label: "四" }, { id: "fri", label: "五" }, { id: "sat", label: "六" }, { id: "sun", label: "日" }];
</script>

<div class="weekday-row"><span class="weekday-label">星期</span><div class="day-picker" role="group" aria-label="选择星期">
  {#each dayOptions as day}
    <label class:day-active={schedule.days.includes(day.id)}>
      <input type="checkbox" aria-label={`周${day.label}`} checked={schedule.days.includes(day.id)} on:change={() => onDay(day.id)} />
      <span aria-hidden="true">{day.label}</span>
    </label>
  {/each}
</div></div>
<div class="time-range">
  <div class="form-row"><label for={`start-${target}`}>开始时间</label><input id={`start-${target}`} type="time" value={schedule.start} on:input={(event) => onTime("start", event.currentTarget.value)} /></div>
  <span class="range-divider">至</span>
  <div class="form-row"><label for={`end-${target}`}>结束时间</label><input id={`end-${target}`} type="time" value={schedule.end} on:input={(event) => onTime("end", event.currentTarget.value)} /></div>
</div>
<small class="field-hint">北京时间 · 开始与结束相同表示全天。</small>

<style>
  .weekday-row { display: flex; align-items: center; gap: 12px; }
  .weekday-label { flex: none; color: var(--foreground); font-size: 12px; font-weight: 600; }
  .day-picker { display: flex; flex-wrap: wrap; gap: 6px; }
  .day-picker label { position: relative; cursor: pointer; }
  .day-picker input { position: absolute; opacity: 0; width: 1px; height: 1px; }
  .day-picker span { box-sizing: border-box; width: 34px; height: 32px; display: grid; place-items: center; border: 1px solid var(--border); border-radius: 4px; background: var(--surface); color: var(--foreground); font-size: 12px; font-weight: 500; transition: background 160ms var(--ease-ui), border-color 160ms var(--ease-ui); }
  .day-picker label:hover span { background: var(--subtle); }
  .day-picker .day-active span, .day-picker .day-active:hover span { border-color: var(--accent); background: var(--accent-soft); color: var(--accent-text); font-weight: 700; }
  .day-picker input:focus-visible + span { outline: 2px solid var(--accent); outline-offset: 2px; }
  .time-range { display: flex; align-items: end; gap: 12px; max-width: 360px; margin-top: 16px; }
  .form-row { display: grid; gap: 7px; flex: 1; min-width: 0; }
  .form-row label { color: var(--foreground); font-size: 12px; font-weight: 600; }
  .form-row input { box-sizing: border-box; width: 100%; min-width: 0; height: 36px; border: 1px solid var(--control-border); border-radius: 5px; background: var(--control); color: var(--foreground); color-scheme: inherit; padding: 0 10px; font: inherit; font-size: 13px; font-variant-numeric: tabular-nums; caret-color: var(--accent); }
  .form-row input:hover { border-color: var(--control-hover); }
  .form-row input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .range-divider { color: var(--muted); font-size: 12px; padding-bottom: 9px; }
  .field-hint { color: var(--muted); font-size: 12px; line-height: 1.5; display: block; margin-top: 9px; }
  @media (prefers-reduced-motion: reduce) { .day-picker span { transition: none; } }
</style>
