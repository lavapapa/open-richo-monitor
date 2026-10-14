import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DesktopSnapshot } from "$lib/desktop-api";
const api = vi.hoisted(() => ({ completeConfigurationChangeIntro: vi.fn(), setUsageStatisticsEnabled: vi.fn(), openExternalUrl: vi.fn(), previewMode: false }));
vi.mock("$lib/desktop-api", () => ({ desktopApi: api }));
import UpgradeFlow from "./UpgradeFlow.svelte";
import { pendingUpgradeItems } from "./upgrade-items";
const snapshot = { setupCompleted: true, configurationChangeIntroPending: true, usageStatisticsIntroPending: true, channels: [{}] } as DesktopSnapshot;
beforeEach(() => { vi.resetAllMocks(); HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); }; });
afterEach(cleanup);

it("跨版本升级按固定顺序显示全部待确认项，并分别保存复选框选择", async () => {
  const onDone = vi.fn().mockResolvedValue(undefined);
  render(UpgradeFlow, { snapshot, onDone });
  expect(screen.getByText("1/2")).toBeTruthy();
  await fireEvent.click(screen.getByRole("checkbox", { name: "启用配置变更提醒" }));
  await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
  await waitFor(() => expect(screen.getByText("2/2")).toBeTruthy());
  expect(api.completeConfigurationChangeIntro).toHaveBeenCalledWith(true);
  const checkbox = screen.getByRole("checkbox", { name: "同意分享使用统计" }) as HTMLInputElement;
  expect(checkbox.checked).toBe(false);
  expect(screen.getByText("监控记录")).toBeTruthy();
  expect(screen.getByText(/用于解决 bug/)).toBeTruthy();
  expect(screen.queryByText("美国节点")).toBeNull();
  await fireEvent.click(screen.getByRole("link", { name: "PostHog" }));
  expect(api.openExternalUrl).toHaveBeenCalledWith("https://posthog.com/");
  await fireEvent.click(screen.getByRole("button", { name: "完成" }));
  expect(api.setUsageStatisticsEnabled).toHaveBeenCalledWith(false);
  await waitFor(() => expect(onDone).toHaveBeenCalledOnce());
});

it("单项、无项与首次引导按持久化待确认状态筛选", () => {
  expect(pendingUpgradeItems({ ...snapshot, configurationChangeIntroPending: false })).toHaveLength(1);
  expect(pendingUpgradeItems({ ...snapshot, configurationChangeIntroPending: false, usageStatisticsIntroPending: false })).toHaveLength(0);
  expect(pendingUpgradeItems({ ...snapshot, setupCompleted: false })).toHaveLength(0);
  expect(pendingUpgradeItems({ ...snapshot, channels: [] }).map(item => item.id)).toEqual(["usage"]);
});

it("失败保留当前步骤，退出保留未确认项", async () => {
  api.completeConfigurationChangeIntro.mockRejectedValueOnce(new Error("写入失败"));
  const onDismiss = vi.fn();
  render(UpgradeFlow, { snapshot, onDone: vi.fn(), onDismiss });
  await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", "写入失败");
  expect(screen.getByText("1/2")).toBeTruthy();
  await fireEvent(screen.getByRole("dialog"), new Event("cancel", { cancelable: true }));
  expect(onDismiss).toHaveBeenCalledOnce();
  expect(api.setUsageStatisticsEnabled).not.toHaveBeenCalled();
});
