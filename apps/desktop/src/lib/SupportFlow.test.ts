import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({ openExternalUrl: vi.fn() }));
vi.mock("$lib/desktop-api", () => ({ desktopApi: api }));
import SupportFlow from "./SupportFlow.svelte";
import appInfo from "../../src-tauri/tauri.conf.json";
beforeEach(() => {
  vi.resetAllMocks();
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); this.dispatchEvent(new Event("close")); };
});
afterEach(cleanup);

it("评价可直接跳过，好评才展示 Star 和打赏作者", async () => {
  const close = vi.fn();
  render(SupportFlow, { onClose: close, fundingUrl: "https://afdian.com/a/example" });
  expect(screen.queryByRole("button", { name: "打赏作者" })).toBeNull();
  await fireEvent.click(screen.getByRole("button", { name: "好评" }));
  const star = screen.getByRole("button", { name: "GitHub Star" });
  expect(star.compareDocumentPosition(screen.getByRole("button", { name: "打赏作者" })) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(screen.getByText("RichoMonitor 是一个完全开源免费的软件，如果帮到了你，欢迎 star/打赏！")).toBeTruthy();
  await fireEvent.click(star);
  expect(api.openExternalUrl).toHaveBeenCalledWith("https://github.com/lavapapa/open-richo-monitor");
  await fireEvent.click(screen.getByRole("button", { name: "打赏作者" }));
  expect(api.openExternalUrl).toHaveBeenCalledWith("https://afdian.com/a/example");
  await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
  expect(close).toHaveBeenCalledOnce();
});

it("意见入口嵌入 Tally，附带版本和系统，页面加载不代表提交成功", async () => {
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)");
  render(SupportFlow, { onClose: vi.fn(), fundingUrl: null, feedbackUrl: "https://tally.so/r/9qL9ME", appVersion: "0.1.4" });
  await fireEvent.click(screen.getByRole("button", { name: "有一些意见" }));
  const frame = screen.getByTitle("RichoMonitor 反馈表单") as HTMLIFrameElement;
  const url = new URL(frame.src);
  expect(url.origin + url.pathname).toBe("https://tally.so/embed/9qL9ME");
  expect(url.searchParams.get("version")).toBe("0.1.4");
  expect(url.searchParams.get("platform")).toBe("macos");
  expect(screen.getByRole("status")).toBeTruthy();
  await fireEvent.load(frame);
  expect(screen.queryByRole("status")).toBeNull();
  expect(screen.queryByText("反馈已收到，谢谢你。")).toBeNull();
});

it("设置入口直接显示同一表单，网页入口保留元数据，打开失败可重试", async () => {
  const close = vi.fn();
  api.openExternalUrl.mockRejectedValueOnce(new Error("无法打开浏览器"));
  render(SupportFlow, { onClose: close, initialStep: "feedback", feedbackUrl: "https://tally.so/r/9qL9ME", appVersion: "0.1.4" });
  const frame = screen.getByTitle("RichoMonitor 反馈表单");
  await fireEvent.click(screen.getByRole("button", { name: "打开网页填写" }));
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", "无法打开浏览器");
  expect(screen.getByTitle("RichoMonitor 反馈表单")).toBe(frame);
  api.openExternalUrl.mockResolvedValueOnce(undefined);
  await fireEvent.click(screen.getByRole("button", { name: "打开网页填写" }));
  const url = new URL(api.openExternalUrl.mock.calls.at(-1)![0]);
  expect(url.pathname).toBe("/r/9qL9ME");
  expect(url.searchParams.get("version")).toBe("0.1.4");
  expect(screen.queryByRole("alert")).toBeNull();
  await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
  expect(close).toHaveBeenCalledOnce();
});

it("Windows 界面附带 Windows 标识，正式配置允许加载 Tally 表单", () => {
  vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Windows NT 10.0; Win64; x64)");
  render(SupportFlow, { onClose: vi.fn(), initialStep: "feedback" });
  const frame = screen.getByTitle("RichoMonitor 反馈表单") as HTMLIFrameElement;
  expect(new URL(frame.src).searchParams.get("platform")).toBe("windows");
  expect(appInfo.app.security.csp).toContain("frame-src https://tally.so");
});
