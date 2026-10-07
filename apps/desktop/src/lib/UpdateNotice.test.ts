import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, expect, it, vi } from "vitest";
import UpdateNotice from "./UpdateNotice.svelte";
import type { UpdateStatus } from "$lib/desktop-api";

afterEach(cleanup);

const status = (overrides: Partial<UpdateStatus> = {}): UpdateStatus => ({
  phase: "idle", version: null, notes: null, downloaded: 0, total: null, error: null, ...overrides,
});

it("顶部更新条显示版本和安装中断提示，不铺开更新说明", () => {
  render(UpdateNotice, { props: { mode: "banner", status: status({ phase: "available", version: "0.2.0", notes: "修复连接稳定性" }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByText("发现新版本 0.2.0")).toBeTruthy();
  expect(screen.queryByText("修复连接稳定性")).toBeNull();
  expect(screen.getByText("安装时会短暂中断监控与通知连接，重启后恢复。")).toBeTruthy();
  expect(screen.getByRole("button", { name: "下载并重启" })).toBeTruthy();
});

it("顶部更新条完整显示可重试的下载错误", () => {
  const error = `签名校验失败：${"证书信息不匹配；".repeat(40)}`;
  render(UpdateNotice, { props: { mode: "banner", status: status({ phase: "available", version: "0.2.0", error }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByRole("alert").textContent).toBe(error);
});

it("设置页通过展开项显示完整更新说明，成功检查显示最新状态", async () => {
  const notes = "修复连接稳定性\n第二行详细说明";
  const view = render(UpdateNotice, { props: { mode: "settings", checked: true, status: status({ phase: "available", version: "0.2.0", notes }), onCheck: vi.fn(), onInstall: vi.fn() } });
  await fireEvent.click(screen.getByText("更新内容"));
  expect(screen.getByText((_, element) => element?.tagName === "PRE" && element.textContent === notes)).toBeTruthy();
  await view.rerender({ mode: "settings", checked: true, status: status(), onCheck: vi.fn(), onInstall: vi.fn() });
  expect(screen.getByText("当前已是最新版本。")).toBeTruthy();
});

it("尚未检查和网页预览显示自动检查说明", () => {
  render(UpdateNotice, { props: { mode: "settings", checked: false, status: status(), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByText("自动检查更新，每小时检查一次。")).toBeTruthy();
});

it("显示下载进度并在安装阶段隐藏重复安装入口", async () => {
  const view = render(UpdateNotice, { props: { mode: "banner", status: status({ phase: "downloading", downloaded: 512, total: 1024 }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByText("50% · 512 B / 1.0 KB")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "下载并重启" })).toBeNull();
  await view.rerender({ status: status({ phase: "installing" }) });
  expect(screen.getByText("正在安装更新…")).toBeTruthy();
  expect(screen.getByText("安装完成后应用将自动重启。")).toBeTruthy();
});

it("设置页展示检查错误并提供重试", async () => {
  const onCheck = vi.fn();
  render(UpdateNotice, { props: { mode: "settings", status: status({ error: "网络不可用" }), onCheck, onInstall: vi.fn() } });
  expect(screen.getByRole("alert").textContent).toBe("网络不可用");
  await fireEvent.click(screen.getByRole("button", { name: "重试检查" }));
  expect(onCheck).toHaveBeenCalledOnce();
});

it("检查中禁用重复点击", async () => {
  render(UpdateNotice, { props: { mode: "settings", status: status({ phase: "checking" }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByRole("button", { name: "检查更新" }).hasAttribute("disabled")).toBe(true);
});
