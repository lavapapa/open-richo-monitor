import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import UpdateNotice from "./UpdateNotice.svelte";
import type { UpdateStatus } from "$lib/desktop-api";

afterEach(cleanup);
beforeEach(() => {
  HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); };
  HTMLDialogElement.prototype.close = function () { this.removeAttribute("open"); };
});

it("等待空闲时保留立即更新入口，默认开启自动更新", async () => {
  const onInstall = vi.fn();
  const onAutoChange = vi.fn();
  render(UpdateNotice, { props: { mode: "settings", status: status({ phase: "waiting" }), onCheck: vi.fn(), onInstall, onAutoChange } });
  expect(screen.getByText("更新已下载，等待监控与扫描空闲后安装。")).toBeTruthy();
  expect(screen.getByRole("button", { name: "立即更新" })).toBeTruthy();
  const toggle = screen.getByRole("checkbox", { name: "监控空闲时自动更新" }) as HTMLInputElement;
  expect(toggle.checked).toBe(true);
  await fireEvent.click(toggle);
  expect(onAutoChange).toHaveBeenCalledWith(false);
  expect(onInstall).not.toHaveBeenCalled();
});

it("侧栏下载入口请求空闲更新", async () => {
  const onInstall = vi.fn();
  render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase: "available" }), onCheck: vi.fn(), onInstall } });
  await fireEvent.click(screen.getByRole("button", { name: "下载更新" }));
  expect(onInstall).toHaveBeenCalledWith(false);
});

const status = (overrides: Partial<UpdateStatus> = {}): UpdateStatus => ({
  phase: "idle", autoInstall: true, version: null, notes: null, downloaded: 0, total: null, error: null, ...overrides,
});

it("侧栏保留简短操作，版本和完整说明留在提示与设置中", () => {
  render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase: "available", autoInstall: true, version: "0.2.0", notes: "修复连接稳定性" }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByRole("button", { name: "下载更新" }).getAttribute("title")).toContain("0.2.0");
  expect(screen.queryByText("修复连接稳定性")).toBeNull();
  expect(screen.getByText("新版本 · 下载")).toBeTruthy();
});

it("侧栏下载错误可重试，完整原因可通过提示查看", async () => {
  const error = `签名校验失败：${"证书信息不匹配；".repeat(40)}`;
  const onInstall = vi.fn();
  render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase: "available", autoInstall: true, version: "0.2.0", error }), onCheck: vi.fn(), onInstall } });
  const retry = screen.getByRole("button", { name: "重试下载" });
  expect(retry.textContent).toBe("更新失败 · 重试");
  expect(retry.getAttribute("title")).toContain(error);
  await fireEvent.click(retry);
  expect(onInstall).toHaveBeenCalledWith(false);
});

it("设置页通过展开项显示完整更新说明，成功检查显示最新状态", async () => {
  const notes = "修复连接稳定性\n第二行详细说明";
  const view = render(UpdateNotice, { props: { mode: "settings", checked: true, status: status({ phase: "available", autoInstall: true, version: "0.2.0", notes }), onCheck: vi.fn(), onInstall: vi.fn() } });
  await fireEvent.click(screen.getByText("更新内容"));
  expect(screen.getByText((_, element) => element?.tagName === "PRE" && element.textContent === notes)).toBeTruthy();
  await view.rerender({ mode: "settings", checked: true, status: status(), onCheck: vi.fn(), onInstall: vi.fn() });
  expect(screen.getByText("暂未发现更新。")).toBeTruthy();
});

it("尚未检查和网页预览显示自动检查说明", () => {
  render(UpdateNotice, { props: { mode: "settings", checked: false, status: status(), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByText("自动检查更新，每小时检查一次。")).toBeTruthy();
});

it("显示下载进度并在安装阶段隐藏重复安装入口", async () => {
  const view = render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase: "downloading", downloaded: 512, total: 1024 }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByText("自动下载 · 50%")).toBeTruthy();
  expect(screen.getByTitle(/512 B \/ 1.0 KB/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "下载更新" })).toBeNull();
  await view.rerender({ status: status({ phase: "installing" }) });
  expect(screen.getByText("正在更新…")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "立即更新" })).toBeNull();
});

it("下载完成后的侧栏立即更新需要确认，取消保持等待", async () => {
  const onInstall = vi.fn();
  render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase: "waiting", version: "0.2.0" }), onCheck: vi.fn(), onInstall } });
  await fireEvent.click(screen.getByRole("button", { name: "立即更新" }));
  expect(screen.getByRole("dialog", { name: "立即更新？" })).toBeTruthy();
  await fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(onInstall).not.toHaveBeenCalled();
  await fireEvent.click(screen.getByRole("button", { name: "立即更新" }));
  await fireEvent.click(screen.getByRole("button", { name: "立即更新并重启" }));
  expect(onInstall).toHaveBeenCalledExactlyOnceWith(true);
});

it.each(["idle", "checking"] as const)("%s 状态不占侧栏空间", (phase) => {
  render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.queryByRole("status")).toBeNull();
});

it("未知下载大小或关闭自动更新时，下载状态仍然如实显示", () => {
  render(UpdateNotice, { props: { mode: "sidebar", status: status({ phase: "downloading", autoInstall: false, downloaded: 512 }), onCheck: vi.fn(), onInstall: vi.fn() } });
  expect(screen.getByText("正在下载")).toBeTruthy();
  expect(screen.queryByText(/%/)).toBeNull();
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
