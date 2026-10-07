import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import source from "./+page.svelte?raw";
const api = vi.hoisted(() => ({ subscribeProminentAlert: vi.fn(), showProminentAlert: vi.fn(), confirmProminentAlertFrame: vi.fn(), dismissProminentAlert: vi.fn(), openExternalUrl: vi.fn() }));
vi.mock("$lib/desktop-api", () => ({ desktopApi: api, prominentImageSrc: (alert: { imagePath: string | null }) => alert.imagePath }));
import Page from "./+page.svelte";
beforeEach(() => { vi.resetAllMocks(); api.dismissProminentAlert.mockResolvedValue({ message: null }); api.showProminentAlert.mockResolvedValue(undefined); api.confirmProminentAlertFrame.mockResolvedValue(undefined); });
afterEach(cleanup);

it("突出提醒遮罩始终完全不透明", () => {
  const screenStyle = source.match(/\.alert-screen\s*\{([^}]+)\}/)?.[1];
  expect(screenStyle).toContain("background: #f5f6f8");
  expect(screenStyle).not.toMatch(/animation|transition|opacity/);
  expect(source).not.toContain("veil-reveal");
});

it.each(["Escape", " ", "Enter"])("真实零库存上架如实呈现，%s关闭当前事件", async (key) => {
  api.subscribeProminentAlert.mockImplementation(async (receive) => { receive({ eventId: 31, presentationId: 1, name: "官翻品 GR IV", productId: "130", stock: 0, price: "8819.00", imageUrl: null, imagePath: "/images/130.jpg", at: "2026-10-06T10:20:00+08:00" }); return () => {}; });
  render(Page);
  await screen.findByRole("heading", { name: "官翻品 GR IV" });
  expect(screen.getByText("库存提醒")).toBeTruthy();
  expect(screen.getByText("库存数量")).toBeTruthy();
  expect(screen.getByText("¥8819.00")).toBeTruthy();
  expect(screen.getByRole("img", { name: "官翻品 GR IV" }).getAttribute("src")).toBe("/images/130.jpg");
  expect(screen.getByRole("button", { name: "关闭" }).textContent).toBe("ESC 关闭");
  await fireEvent.keyDown(window, { key });
  expect(api.dismissProminentAlert).toHaveBeenCalledExactlyOnceWith(31);
});

it("测试复用上架通知内容，关闭失败可重试", async () => {
  api.subscribeProminentAlert.mockImplementation(async (receive) => { receive({ eventId: -31, presentationId: 1, name: "官翻品 GR IIIx", productId: "65", stock: 3, price: null, imageUrl: null, imagePath: null, at: "" }); return () => {}; });
  api.dismissProminentAlert.mockRejectedValueOnce(new Error("关闭失败"));
  render(Page);
  await screen.findByRole("heading", { name: "官翻品 GR IIIx" });
  expect(screen.getByText("库存提醒")).toBeTruthy();
  expect(screen.queryByText(/测试演示|不发送渠道|演示库存|这是全屏提醒/)).toBeNull();
  expect(screen.queryByRole("button", { name: /打开理光商城/ })).toBeNull();
  expect(screen.getByText("待更新")).toBeTruthy();
  expect(screen.queryByText(/Invalid Date/)).toBeNull();
  await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
  await screen.findByRole("alert");
  await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
  await waitFor(() => expect(api.dismissProminentAlert).toHaveBeenCalledTimes(2));
});

it("预加载页面提交商品内容后显示，连续提醒重置关闭状态并更新内容", async () => {
  let receive!: (alert: any) => void;
  api.subscribeProminentAlert.mockImplementation(async (callback) => { receive = callback; return () => {}; });
  api.showProminentAlert.mockImplementation(async (id) => {
    expect(screen.getByRole("heading", { name: id === 31 ? "GR IV" : "GR IIIx" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "关闭" }) as HTMLButtonElement).disabled).toBe(false);
  });
  render(Page);
  await waitFor(() => expect(receive).toBeTypeOf("function"));
  receive({ eventId: 31, presentationId: 1, name: "GR IV", stock: 0, price: "8819", imageUrl: null, imagePath: null, at: "2026-10-07T10:00:00+08:00" });
  await waitFor(() => expect(api.showProminentAlert).toHaveBeenCalledWith(31, 1));
  await fireEvent.keyDown(window, { key: "Escape" });
  receive({ eventId: 32, presentationId: 2, name: "GR IIIx", stock: 8, price: "6749", imageUrl: null, imagePath: null, at: "2026-10-07T10:00:01+08:00" });
  await waitFor(() => expect(api.showProminentAlert).toHaveBeenCalledWith(32, 2));
  expect(screen.queryByRole("heading", { name: "GR IV" })).toBeNull();
  expect(screen.getByText("¥6749")).toBeTruthy();
  await fireEvent.keyDown(window, { key: "Enter" });
  expect(api.dismissProminentAlert).toHaveBeenLastCalledWith(32);
});

it("旧交付的显示失败不覆盖下一条成功提醒", async () => {
  let receive!: (alert: any) => void;
  let rejectOld!: (cause: Error) => void;
  api.subscribeProminentAlert.mockImplementation(async (callback) => { receive = callback; return () => {}; });
  api.showProminentAlert.mockImplementationOnce(() => new Promise((_, reject) => { rejectOld = reject; })).mockResolvedValue(undefined);
  render(Page);
  await waitFor(() => expect(receive).toBeTypeOf("function"));
  const event = { eventId: 31, name: "GR IV", stock: 0, imagePath: null, imageUrl: null, price: null, at: "" };
  receive({ ...event, presentationId: 1 });
  await waitFor(() => expect(rejectOld).toBeTypeOf("function"));
  receive({ ...event, presentationId: 2, stock: 8 });
  await waitFor(() => expect(api.showProminentAlert).toHaveBeenCalledWith(31, 2));
  rejectOld(new Error("提醒已更新。"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.getByRole("button", { name: "完成" }).hasAttribute("disabled")).toBe(false);
});

it("本地图片解码未完成也立即显示必要内容", async () => {
  const decode = vi.fn(() => new Promise<void>(() => {}));
  Object.defineProperty(HTMLImageElement.prototype, "decode", { configurable: true, value: decode });
  try {
    api.subscribeProminentAlert.mockImplementation(async (receive) => {
      receive({ eventId: 31, presentationId: 1, name: "GR IV", stock: 2, price: null, imagePath: "/cached/31.jpg", imageUrl: null, at: "" });
      return () => {};
    });
    render(Page);
    await waitFor(() => expect(api.showProminentAlert).toHaveBeenCalledExactlyOnceWith(31, 1));
    expect(screen.getByRole("img", { name: "GR IV" }).getAttribute("src")).toBe("/cached/31.jpg");
    expect(screen.getByRole("heading", { name: "GR IV" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "关闭" }).hasAttribute("disabled")).toBe(false);
  } finally { delete (HTMLImageElement.prototype as any).decode; }
});


it("显示命令尚未完成时不确认当前页面帧", async () => {
  let finishShow!: () => void;
  api.showProminentAlert.mockImplementation(() => new Promise<void>((resolve) => { finishShow = resolve; }));
  api.subscribeProminentAlert.mockImplementation(async (receive) => {
    receive({ eventId: 77, presentationId: 3, waitForFrame: true, name: "GR IV", stock: 2, price: null, imagePath: null, imageUrl: null, at: "" });
    return () => {};
  });
  render(Page);
  await waitFor(() => expect(finishShow).toBeTypeOf("function"));
  expect(api.confirmProminentAlertFrame).not.toHaveBeenCalled();
  finishShow();
  await waitFor(() => expect(api.confirmProminentAlertFrame).toHaveBeenCalledExactlyOnceWith(77, 3));
});
