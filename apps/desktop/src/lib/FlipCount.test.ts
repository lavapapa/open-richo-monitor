import { cleanup, fireEvent, render, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const library = vi.hoisted(() => ({ create: vi.fn(), update: vi.fn(), finish: vi.fn(), refresh: vi.fn(), destroy: vi.fn() }));
const native = vi.hoisted(() => ({
  enabled: false, visible: true, focused: true, minimized: false,
  focus: null as null | ((event: { payload: boolean }) => void),
  unlisten: vi.fn(),
}));
vi.mock("@kitlangton/rolling-number", () => ({ createRollingNumber: library.create }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => native.enabled }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({
  isVisible: async () => native.visible,
  isFocused: async () => native.focused,
  isMinimized: async () => native.minimized,
  onFocusChanged: async (handler: typeof native.focus) => { native.focus = handler; return native.unlisten; },
}) }));

import FlipCount from "./FlipCount.svelte";
const originalAnimate = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "animate");

beforeEach(() => {
  vi.spyOn(document, "hasFocus").mockReturnValue(true);
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  native.enabled = false; native.visible = true; native.focused = true; native.minimized = false;
  native.focus = null;
  library.create.mockReturnValue({ update: library.update, finish: library.finish, refresh: library.refresh, destroy: library.destroy });
});
afterEach(() => {
  cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals();
  if (originalAnimate) Object.defineProperty(HTMLElement.prototype, "animate", originalAnimate);
  else Reflect.deleteProperty(HTMLElement.prototype, "animate");
});

describe("今日检查翻页计数", () => {
  it("使用现成库的逐数字翻牌模式，以完整目标值支持进位和大幅跳变", async () => {
    const { container, rerender } = render(FlipCount, { count: 708 });
    await new Promise((resolve) => requestAnimationFrame(resolve));
    expect(library.create).toHaveBeenCalledWith(container.querySelector(".flip-count"), expect.objectContaining({
      value: 708, mode: "flap", locales: "zh-CN", format: { useGrouping: true },
    }));
    await rerender({ count: 709 });
    await rerender({ count: 710 });
    await rerender({ count: 800 });
    expect(library.update.mock.calls.map(([options]) => options.value)).toEqual([709, 710, 800]);
    expect(container.querySelector(".flip-count")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("切换到其他应用时冻结已展示值，返回时一次翻到最新值", async () => {
    const { rerender } = render(FlipCount, { count: 708 });
    await fireEvent(window, new Event("blur"));
    await rerender({ count: 709 });
    await rerender({ count: 710 });
    await rerender({ count: 800 });
    expect(library.update).not.toHaveBeenCalled();
    await fireEvent(window, new Event("focus"));
    await waitFor(() => expect(library.update.mock.calls.map(([options]) => options.value)).toEqual([800]));
  });

  it("最小化或隐藏时保留最新值，恢复可见且获得焦点才更新", async () => {
    let hidden = false;
    vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
    const { rerender } = render(FlipCount, { count: 708 });
    hidden = true;
    await fireEvent(document, new Event("visibilitychange"));
    await rerender({ count: 709 });
    await fireEvent(window, new Event("blur"));
    hidden = false;
    await fireEvent(document, new Event("visibilitychange"));
    await rerender({ count: 710 });
    expect(library.update).not.toHaveBeenCalled();
    await fireEvent(window, new Event("focus"));
    await waitFor(() => expect(library.update).toHaveBeenCalledExactlyOnceWith({ value: 710 }));
  });

  it("原生窗口失焦或关闭隐藏后，恢复时检查可见、最小化与焦点状态", async () => {
    native.enabled = true;
    const { rerender } = render(FlipCount, { count: 700 });
    await waitFor(() => expect(native.focus).not.toBeNull());
    await new Promise((resolve) => setTimeout(resolve));
    native.focused = false;
    native.focus!({ payload: false });
    await rerender({ count: 701 });
    native.visible = false;
    native.focused = true;
    native.focus!({ payload: true });
    await rerender({ count: 702 });
    expect(library.update).not.toHaveBeenCalled();
    native.visible = true;
    native.focus!({ payload: true });
    await waitFor(() => expect(library.update).toHaveBeenCalledExactlyOnceWith({ value: 702 }));
  });

  it("卸载释放动画、窗口与文档监听", async () => {
    native.enabled = true;
    const { unmount } = render(FlipCount, { count: 700 });
    await waitFor(() => expect(native.focus).not.toBeNull());
    const removeWindow = vi.spyOn(window, "removeEventListener");
    const removeDocument = vi.spyOn(document, "removeEventListener");
    unmount();
    expect(library.destroy).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(native.unlisten).toHaveBeenCalledTimes(1));
    expect(removeWindow.mock.calls.map(([name]) => name)).toEqual(expect.arrayContaining(["focus", "blur"]));
    expect(removeDocument).toHaveBeenCalledWith("visibilitychange", expect.any(Function));
    await fireEvent(window, new Event("focus"));
    expect(library.update).not.toHaveBeenCalled();
  });
});

describe("翻牌库实际渲染", () => {
  async function realCounter(value: number, reduced = false) {
    vi.stubGlobal("matchMedia", () => ({ matches: reduced, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
    const cancel = vi.fn();
    Object.defineProperty(HTMLElement.prototype, "animate", { configurable: true, value: vi.fn(() => ({
      currentTime: 0, playState: "running", cancel, onfinish: null,
    })) });
    const style = window.getComputedStyle.bind(window);
    vi.spyOn(window, "getComputedStyle").mockImplementation((element) => element.classList.contains("rn-measure")
      ? { width: "21px", height: "15.4px", direction: "ltr", getPropertyValue: () => "0" } as unknown as CSSStyleDeclaration
      : style(element));
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      const token = this.classList.contains("rn-token");
      const left = token ? Array.from(this.parentElement!.children).indexOf(this) * 7 : 0;
      return { width: token ? 7 : 21, height: 15.4, left, top: 0, right: left + 21, bottom: 15.4, x: left, y: 0, toJSON: () => ({}) };
    });
    const actual = await vi.importActual<typeof import("@kitlangton/rolling-number")>("@kitlangton/rolling-number");
    const host = document.createElement("span");
    document.body.append(host);
    const counter = actual.createRollingNumber(host, { value, mode: "flap", locales: "zh-CN", format: { useGrouping: true }, flipDuration: 220 });
    await new Promise((resolve) => requestAnimationFrame(resolve));
    return { host, counter, cancel };
  }

  it.each([
    [708, 709, ["digit:0"]],
    [709, 710, ["digit:1", "digit:0"]],
    [700, 800, ["digit:2"]],
  ] as const)("%i → %i 仅变动数位生成铰链半卡", async (from, to, keys) => {
    const { host, counter, cancel } = await realCounter(from);
    counter.update({ value: to });
    await new Promise((resolve) => requestAnimationFrame(resolve));
    const changing = Array.from(host.querySelectorAll<HTMLElement>(".rn-slot"))
      .filter((slot) => slot.querySelector(".rn-flap"))
      .map((slot) => slot.dataset.rnKey);
    expect(changing).toEqual(keys);
    counter.destroy();
    expect(host.textContent).toBe(String(to));
    expect(host.querySelector(".rn-visual")).toBeNull();
    expect(cancel).toHaveBeenCalled();
    host.remove();
  });

  it("大数保留完整整数和千位分隔，跨位数进位后仍可读取", async () => {
    const { host, counter } = await realCounter(99_999, true);
    expect(host.querySelector(".rn-value")?.textContent).toBe("99,999");
    counter.update({ value: 999_999_999_999 });
    expect(host.querySelector(".rn-value")?.textContent).toBe("999,999,999,999");
    counter.destroy();
    expect(host.textContent).toBe("999,999,999,999");
    host.remove();
  });

  it("减少动态效果时直接呈现最新数值", async () => {
    const { host, counter } = await realCounter(708, true);
    counter.update({ value: 800 });
    expect(host.querySelector(".rn-flap")).toBeNull();
    expect(host.querySelector(".rn-value")?.textContent).toBe("800");
    counter.destroy();
    host.remove();
  });

  it("组件在隐藏后重建最后展示值，再从700一次翻到800", async () => {
    const setup = await realCounter(700);
    setup.counter.destroy(); setup.host.remove();
    const actual = await vi.importActual<typeof import("@kitlangton/rolling-number")>("@kitlangton/rolling-number");
    library.create.mockImplementation(actual.createRollingNumber);
    let hidden = false;
    vi.spyOn(document, "hidden", "get").mockImplementation(() => hidden);
    const { container, rerender } = render(FlipCount, { count: 700 });
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await fireEvent(window, new Event("blur"));
    hidden = true;
    await fireEvent(document, new Event("visibilitychange"));
    await rerender({ count: 701 });
    await rerender({ count: 800 });
    expect(container.querySelector(".rn-value")?.textContent).toBe("700");
    hidden = false;
    await fireEvent(document, new Event("visibilitychange"));
    await fireEvent(window, new Event("focus"));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await new Promise((resolve) => requestAnimationFrame(resolve));
    expect(container.querySelector(".rn-value")?.textContent).toBe("800");
    expect(container.querySelectorAll("[data-rn-key='digit:2'] .rn-flap").length).toBeGreaterThan(0);
    expect(container.querySelectorAll("[data-rn-key='digit:1'] .rn-flap").length).toBe(0);
    expect(container.querySelectorAll("[data-rn-key='digit:0'] .rn-flap").length).toBe(0);
  });
});
