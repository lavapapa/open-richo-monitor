/*!
 * @license Feel v0.2 — MIT License
 * Copyright (c) 2026 diip3sh
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

/**
 * Feel 的 precise 模型：m=1、f=4.5Hz、ζ=1，k≈799.44、c≈56.55。
 * 临界阻尼让按钮在约 206ms 内进入 2% 误差带，按下最多缩至 0.96。
 * 保留实时速度，中途松开直接反向；反馈与业务点击独立。
 * 临界阻尼解析式取自 Feel v0.2 / response，MIT © 2026 diip3sh。
 */
export const pressModel = { mass: 1, frequency: 4.5, dampingRatio: 1 } as const;
type MotionState = { x: number; v: number };
const omega = 2 * Math.PI * pressModel.frequency;

export function springStep(state: MotionState, target: number, seconds: number): MotionState {
  const displacement = state.x - target;
  const coefficient = state.v + omega * displacement;
  const decay = Math.exp(-omega * seconds);
  return {
    x: target + decay * (displacement + coefficient * seconds),
    v: decay * (coefficient - omega * (displacement + coefficient * seconds)),
  };
}

/** 一个页面级委托，空闲时没有动画帧，导航和列表使用静态反馈。 */
export function pressFeedback(host: HTMLElement) {
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");
  const bodies = new Map<HTMLButtonElement, { state: MotionState; target: number; at: number }>();
  let frame = 0;
  const command = (target: EventTarget | null) => target instanceof Element
    ? target.closest<HTMLButtonElement>("button.primary, button.secondary, button.danger-button, button.support-button, button.star-button")
    : null;
  function advance(body: { state: MotionState; target: number; at: number }, at: number) {
    body.state = springStep(body.state, body.target, Math.max(0, at - body.at) / 1000);
    body.at = at;
  }
  function draw(at: number) {
    frame = 0;
    for (const [button, body] of bodies) {
      if (!button.isConnected || button.disabled) {
        button.style.removeProperty("scale");
        bodies.delete(button);
        continue;
      }
      advance(body, at);
      const settled = Math.abs(body.state.x - body.target) < .001 && Math.abs(body.state.v) < .01;
      button.style.scale = String(1 - .04 * (settled ? body.target : body.state.x));
      if (settled && body.target === 0) {
        button.style.removeProperty("scale");
        bodies.delete(button);
      }
    }
    if ([...bodies.values()].some((body) => Math.abs(body.state.x - body.target) >= .001 || Math.abs(body.state.v) >= .01)) frame = window.requestAnimationFrame(draw);
  }
  function move(button: HTMLButtonElement, target: number) {
    if (reduced.matches || button.disabled) return;
    const now = performance.now();
    const body = bodies.get(button) ?? { state: { x: 0, v: 0 }, target: 0, at: now };
    advance(body, now);
    body.target = target;
    bodies.set(button, body);
    if (!frame) frame = window.requestAnimationFrame(draw);
  }
  const down = (event: PointerEvent) => { const button = command(event.target); if (event.button === 0 && button) move(button, 1); };
  const release = () => { for (const button of bodies.keys()) move(button, 0); };
  const keyDown = (event: KeyboardEvent) => {
    if (event.repeat || event.isComposing || event.metaKey || event.ctrlKey || event.altKey) return;
    const button = command(event.target);
    if ((event.key === " " || event.key === "Enter") && button) move(button, 1);
  };
  const keyUp = (event: KeyboardEvent) => { if (event.key === " " || event.key === "Enter") release(); };
  const reset = () => {
    if (frame) window.cancelAnimationFrame(frame);
    frame = 0;
    for (const button of bodies.keys()) button.style.removeProperty("scale");
    bodies.clear();
  };
  const visibility = () => { if (document.hidden) reset(); };
  host.addEventListener("pointerdown", down);
  host.addEventListener("keydown", keyDown);
  window.addEventListener("pointerup", release);
  window.addEventListener("pointercancel", release);
  window.addEventListener("keyup", keyUp);
  window.addEventListener("blur", reset);
  document.addEventListener("visibilitychange", visibility);
  reduced.addEventListener("change", reset);
  return { destroy() {
    reset();
    host.removeEventListener("pointerdown", down);
    host.removeEventListener("keydown", keyDown);
    window.removeEventListener("pointerup", release);
    window.removeEventListener("pointercancel", release);
    window.removeEventListener("keyup", keyUp);
    window.removeEventListener("blur", reset);
    document.removeEventListener("visibilitychange", visibility);
    reduced.removeEventListener("change", reset);
  } };
}
