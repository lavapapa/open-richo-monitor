import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({ openExternalUrl: vi.fn(), submitFeedback: vi.fn() }));
vi.mock("$lib/desktop-api", () => ({ desktopApi: api }));
import SupportFlow from "./SupportFlow.svelte";
beforeEach(() => {
  vi.resetAllMocks();
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); this.dispatchEvent(new Event("close")); };
});
afterEach(cleanup);

it("评价可直接跳过，好评才展示 Star 和打赏作者", async () => {
  const close = vi.fn();
  render(SupportFlow, { onClose: close, fundingUrl: "https://afdian.com/a/example", feedbackUrl: null });
  expect(screen.queryByRole("button", { name: "打赏作者" })).toBeNull();
  await fireEvent.click(screen.getByRole("button", { name: "好评" }));
  await fireEvent.click(screen.getByRole("button", { name: "打赏作者" }));
  expect(api.openExternalUrl).toHaveBeenCalledWith("https://afdian.com/a/example");
  await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
  expect(close).toHaveBeenCalledOnce();
});

it("反馈提交失败保留内容，成功后才确认收到", async () => {
  api.submitFeedback.mockRejectedValueOnce(new Error("网络超时，请重试。"));
  render(SupportFlow, { onClose: vi.fn(), fundingUrl: null, feedbackUrl: "https://formspree.io/f/example" });
  await fireEvent.click(screen.getByRole("button", { name: "有一些意见" }));
  await fireEvent.input(screen.getByRole("textbox", { name: "意见" }), { target: { value: "二维码需要更清楚的提示" } });
  await fireEvent.click(screen.getByRole("button", { name: "提交反馈" }));
  await screen.findByRole("alert");
  expect((screen.getByRole("textbox", { name: "意见" }) as HTMLTextAreaElement).value).toBe("二维码需要更清楚的提示");
  expect(screen.queryByText("反馈已收到，谢谢你。" )).toBeNull();
  api.submitFeedback.mockResolvedValueOnce(undefined);
  await fireEvent.click(screen.getByRole("button", { name: "提交反馈" }));
  await screen.findByText("反馈已收到，谢谢你。");
});
