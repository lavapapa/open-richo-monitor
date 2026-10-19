import { afterEach, describe, expect, it, vi } from "vitest";
import { pressFeedback, pressModel, springStep } from "./press-feedback";

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("精确的按钮触感", () => {
  it("使用 Feel precise 的临界阻尼参数，单向到达且没有回弹", () => {
    expect(pressModel.mass).toBe(1);
    expect(pressModel.frequency).toBe(4.5);
    expect(pressModel.dampingRatio).toBe(1);
    let previous = 0;
    for (let t = 0; t <= .6; t += .01) {
      const state = springStep({ x: 0, v: 0 }, 1, t);
      expect(state.x).toBeGreaterThanOrEqual(previous);
      expect(state.x).toBeLessThanOrEqual(1);
      previous = state.x;
    }
    expect(springStep({ x: 0, v: 0 }, 1, .21).x).toBeGreaterThan(.98);
  });

  it("中途松开沿用当下位置和速度", () => {
    const pressed = springStep({ x: 0, v: 0 }, 1, .04);
    const interrupted = springStep(pressed, 0, 0);
    expect(interrupted.x).toBeCloseTo(pressed.x, 12);
    expect(interrupted.v).toBeCloseTo(pressed.v, 12);
    const released = springStep(pressed, 0, .4);
    expect(Math.abs(released.x)).toBeLessThan(.001);
  });

  it("减少动态效果时保持静止，点击语义不受影响", () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: true, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
    const raf = vi.spyOn(window, "requestAnimationFrame");
    const host = document.createElement("div");
    host.innerHTML = '<button class="secondary">取消</button>';
    document.body.append(host);
    const button = host.querySelector("button")!;
    const click = vi.fn();
    button.addEventListener("click", click);
    const action = pressFeedback(host);
    button.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));
    button.click();
    expect(click).toHaveBeenCalledOnce();
    expect(button.style.scale).toBe("");
    expect(raf).not.toHaveBeenCalled();
    action.destroy();
  });

  it("按键在按钮外松开也会回位，销毁会清理待绘制帧", () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() })));
    const raf = vi.spyOn(window, "requestAnimationFrame").mockReturnValue(17);
    const cancel = vi.spyOn(window, "cancelAnimationFrame");
    const host = document.createElement("div");
    host.innerHTML = '<button class="secondary">取消</button><button class="primary" disabled>保存</button>';
    document.body.append(host);
    const action = pressFeedback(host);
    host.querySelector("button:disabled")!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));
    expect(raf).not.toHaveBeenCalled();
    host.querySelector("button")!.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true }));
    expect(raf).toHaveBeenCalledOnce();
    window.dispatchEvent(new KeyboardEvent("keyup", { key: " " }));
    action.destroy();
    expect(cancel).toHaveBeenCalledWith(17);
    expect(host.querySelector("button")!.style.scale).toBe("");
  });
});
