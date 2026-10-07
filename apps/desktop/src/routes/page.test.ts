import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopSnapshot, NotificationChannel, ProductRecord, UpdateStatus } from "$lib/desktop-api";

vi.mock("canvas-confetti", () => ({ default: { create: () => Object.assign(vi.fn(), { reset: vi.fn() }) } }));
vi.mock("qrcode", () => ({ default: { toDataURL: vi.fn(async () => "data:image/png;base64,cXI=") } }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({
  isVisible: async () => true,
  isFocused: async () => true,
  isMinimized: async () => false,
  onFocusChanged: async () => vi.fn(),
}) }));

const bridge = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(() => true),
  refreshCatalog: vi.fn(async () => ({ message: null })),
  eventHandler: null as null | ((event: { payload: DesktopSnapshot }) => void),
  updateHandler: null as null | ((event: { payload: UpdateStatus }) => void),
  updateUnlisten: vi.fn(),
  installUpdateCount: 0,
  installUpdatePromise: null as null | Promise<void>,
}));

vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn(async () => "0.1.0") }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: (name: string, args?: unknown) => name === "refresh_catalog_metadata" ? bridge.refreshCatalog() : name === "check_for_updates" ? Promise.resolve({ phase: "idle", version: null, notes: null, downloaded: 0, total: null, error: null }) : name === "install_update" ? (bridge.installUpdateCount++, bridge.installUpdatePromise ?? Promise.resolve()) : bridge.invoke(name, args), isTauri: bridge.isTauri, convertFileSrc: (path: string) => `asset://localhost/${path}` }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: any }) => void) => {
    if (name === "desktop-state-changed") bridge.eventHandler = handler;
    if (name === "desktop-update") { bridge.updateHandler = handler; bridge.updateUnlisten = vi.fn(); return bridge.updateUnlisten; }
    return vi.fn();
  }),
}));

import Page from "./+page.svelte";
import pageSource from "./+page.svelte?raw";
import tileSource from "$lib/ProductTile.svelte?raw";

function makeSnapshot(overrides: Partial<DesktopSnapshot> = {}): DesktopSnapshot {
  return {
    setupCompleted: true,
    systemNotificationsEnabled: false,
    systemNotificationDelivery: null,
    runtime: {
      state: "stopped", nextStartAt: null, lastSuccessAt: null, lastError: null,
    },
    config: {
      autoStartMonitoring: true,
      monitoringMode: "listed_products",
      schedule: { days: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"], start: "09:00", end: "19:00" },
      rate: { intervalMinMs: 1000, intervalMaxMs: 2000, failuresBeforeBackoff: 3, failureBackoffSeconds: 20 },
      useSystemProxy: true, useProxyPool: false, failureAlertAfterMinutes: 10,
    },
    products: [], catalog: [], channels: [], providers: [], proxies: [], scan: null,
    platform: {
      loginStartEnabled: false,
      notificationPermission: "prompt", notificationPermissionError: null, projectUrl: null, tutorialUrl: null, feedbackUrl: null,
    },
    recentEvents: [], recentChecks: [],
    ...overrides,
  };
}

const product: ProductRecord = {
  productId: "245", name: "GR IV HDF", enabled: true, prominentAlert: false, checkCount: 37, runtimeError: null,
  observation: { availability: "out_of_stock", isShow: 0, stock: 0, checkedAt: null },
  metadata: null, imagePath: null, metadataUpdatedAt: null,
  todayCheckCount: 0, todaySuccessCount: 0, todayFailureCount: 0, monitoringMs: 0,
};

const channel: NotificationChannel = {
  id: "channel-1", name: "工作群", providerId: "feishu", providerName: "飞书", enabled: true,
  configuredFieldKeys: ["appId"], subscriptions: ["stock_available"], lastTest: null, lastDelivery: null,
};

const onboardingCatalog: ProductRecord[] = [
  ["19", "官翻品 GR III ING 套装版本"], ["65", "官翻品 GR IIIx"],
  ["66", "官翻品 RICOH GR III"], ["130", "官翻品 GR IV"],
  ["245", "官翻品 GR IV HDF"], ["9", "RICOH GR IIIx"],
  ["108", "GR SPACE VIP会员卡"],
].map(([productId, name]) => ({ ...product, productId, name, enabled: false }));

let current: DesktopSnapshot;

function messagePage(query: { date: string | null; productId: string | null; limit: number }) {
  const recorded = new Set(current.recentChecks.map((item) => `${item.productId}|${item.at}`));
  const items = [
    ...current.recentChecks.map((item) => ({ at: item.at, productId: item.productId, name: item.name, isShow: item.isShow, stock: item.stock, detail: `检查结果：${item.isShow === 1 ? "已上架" : "未上架"}` })),
    ...current.recentEvents.filter((item) => item.kind !== "stock_available" || !recorded.has(`${item.productId}|${item.at}`)).map((item) => ({ at: item.at, productId: item.productId, name: current.products.find((product) => product.productId === item.productId)?.name ?? `商品 ${item.productId}`, isShow: null, stock: null, detail: "商品有货" })),
  ].filter((item) => (!query.date || new Date(Date.parse(item.at) + 8 * 3600_000).toISOString().slice(0, 10) === query.date) && (!query.productId || item.productId === query.productId));
  return { items: items.sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, query.limit), nextCursor: null };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function useOnboardingBridge() {
  bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
    if (name === "get_desktop_snapshot") return structuredClone(current);
    if (name === "check_for_updates") return { phase: "idle", version: null, notes: null, downloaded: 0, total: null, error: null };
    if (name === "install_update") return undefined;
    if (name === "query_messages") return messagePage(args!.query!);
    if (name === "set_onboarding_products") {
      const all = [...current.products, ...current.catalog];
      current.products = args!.productIds.map((id: string) => ({ ...all.find((item) => item.productId === id)!, enabled: true }));
      current.catalog = all.filter((item) => !args!.productIds.includes(item.productId)).map((item) => ({ ...item, enabled: false }));
      return { message: null };
    }
    if (name === "request_notification_permission") {
      current.platform!.notificationPermission = "granted";
      return structuredClone(current.platform);
    }
    if (name === "refresh_notification_permission") return structuredClone(current.platform);
    if (name === "set_system_notifications_enabled") current.systemNotificationsEnabled = args!.enabled;
    else if (name === "set_login_start") {
      current.platform!.loginStartEnabled = args!.enabled;
      return structuredClone(current.platform);
    } else if (name === "save_monitoring_config") current.config = structuredClone(args!.config);
    else if (name === "set_auto_start_monitoring") current.config!.autoStartMonitoring = args!.enabled;
    else if (name === "complete_setup") {
      current.setupCompleted = true;
      current.runtime!.state = current.config!.autoStartMonitoring ? "monitoring" : "stopped";
    } else if (name === "save_notification_channel") {
      const added = { ...channel, id: "saved-channel", name: args!.channel.name };
      current.channels.push(added);
      return added;
    } else if (name === "test_notification_channel") return { outcome: "accepted", message: "平台已接受测试消息" };
    else if (name === "set_notification_channel_enabled") current.channels.find((item) => item.id === args!.channelId)!.enabled = args!.enabled;
    else if (name === "test_system_notification") return "accepted_by_system";
    else throw new Error(`Unexpected command: ${name}`);
    return { message: null };
  });
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  localStorage.clear();
  delete document.documentElement.dataset.theme;
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("message-scroll") ? 360 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("message-scroll") ? 460 : 0;
  });
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    configurable: true, value() { this.setAttribute("open", ""); },
  });
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    configurable: true, value() { this.removeAttribute("open"); this.dispatchEvent(new Event("close")); },
  });
  current = makeSnapshot();
  bridge.eventHandler = null;
  bridge.updateHandler = null;
  bridge.installUpdateCount = 0;
  bridge.installUpdatePromise = null;
  bridge.isTauri.mockReturnValue(true);
  bridge.refreshCatalog.mockClear();
  bridge.invoke.mockImplementation(async (name: string, args?: { query?: { date: string | null; productId: string | null; limit: number } }) => {
    if (name === "get_desktop_snapshot") return structuredClone(current);
    if (name === "query_messages") return messagePage(args!.query!);
    if (name === "test_system_notification") return "accepted_by_system";
    throw new Error(`Unexpected command: ${name}`);
  });
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("桌面主流程", () => {
  it("撤销系统通知权限显示持续提示，检查后恢复授权状态", async () => {
    current.systemNotificationsEnabled = true;
    current.platform!.notificationPermission = "denied";
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "refresh_notification_permission") {
        current.platform!.notificationPermission = "granted";
        return structuredClone(current.platform);
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    expect(screen.getByRole("alert").textContent).toContain("系统通知权限已关闭");
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.queryByRole("button", { name: "申请权限" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "检查权限" }));
    await screen.findByText("权限：已允许");
    expect(screen.queryByText(/系统通知权限已关闭/)).toBeNull();
    expect(current.systemNotificationsEnabled).toBe(true);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "request_notification_permission")).toBe(false);
  });

  it("尚未决定通知权限可申请，拒绝后引导系统设置并停止重复申请", async () => {
    current.platform!.notificationPermission = "prompt";
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "request_notification_permission") {
        current.platform!.notificationPermission = "denied";
        return structuredClone(current.platform);
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "申请权限" }));
    await screen.findByText("权限：已关闭");
    expect(screen.queryByRole("button", { name: "申请权限" })).toBeNull();
    expect(screen.getByText(/在系统设置中开启本应用通知/)).toBeTruthy();
  });
  it.each(["status", "products", "detail"])("首次从 %s 开启铃铛先测试，取消保留关闭状态", async (origin) => {
    current.products = [product];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    if (origin !== "status") await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    if (origin === "detail") await fireEvent.click(screen.getByRole("button", { name: `查看${product.name}详情` }));
    await fireEvent.click(screen.getByRole("button", { name: origin === "detail" ? "商品详情突出提醒" : `突出提醒${product.name}` }));
    const confirmation = await screen.findByRole("dialog", { name: "先体验突出提醒" });
    expect(within(confirmation).getByText(product.name)).toBeTruthy();
    expect(bridge.invoke.mock.calls.some(([name]) => name === "set_product_prominent_alert")).toBe(false);
    await fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("dialog", { name: "先体验突出提醒" })).toBeNull();
    expect(current.products[0].prominentAlert).toBe(false);
    expect(localStorage.getItem("rm.prominent.tested")).toBeNull();
  });

  it("首次铃铛确认的 Escape 不关闭底层商品详情", async () => {
    current.products = [structuredClone(product)];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: `查看${product.name}详情` }));
    await fireEvent.click(screen.getByRole("button", { name: "商品详情突出提醒" }));
    const confirmation = await screen.findByRole("dialog", { name: "先体验突出提醒" });
    await fireEvent.keyDown(confirmation, { key: "Escape" });
    await fireEvent(confirmation, new Event("cancel", { cancelable: true }));
    expect(screen.getByRole("dialog", { name: "商品详情" })).toBeTruthy();
    expect(screen.queryByRole("dialog", { name: "先体验突出提醒" })).toBeNull();
    expect(current.products[0].prominentAlert).toBe(false);
  });

  it("首次铃铛测试指定商品，成功后开启并跨重开免确认", async () => {
    current.products = [structuredClone(product)];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "test_prominent_alert") return { message: null };
      if (name === "set_product_prominent_alert") {
        current.products[0].prominentAlert = args!.enabled;
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    const view = render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: `突出提醒${product.name}` }));
    const confirmation = await screen.findByRole("dialog", { name: "先体验突出提醒" });
    await fireEvent.click(within(confirmation).getByRole("button", { name: "测试" }));
    await waitFor(() => expect(current.products[0].prominentAlert).toBe(true));
    expect(bridge.invoke).toHaveBeenCalledWith("test_prominent_alert", { productId: product.productId });
    expect(localStorage.getItem("rm.prominent.tested")).toBe("true");
    expect(screen.queryByRole("dialog", { name: "先体验突出提醒" })).toBeNull();
    view.unmount();
    current.products[0].prominentAlert = false;
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: `突出提醒${product.name}` }));
    await waitFor(() => expect(current.products[0].prominentAlert).toBe(true));
    expect(screen.queryByRole("dialog", { name: "先体验突出提醒" })).toBeNull();
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "test_prominent_alert")).toHaveLength(1);
  });

  it("突出提醒测试失败保留确认和关闭状态，可重试", async () => {
    current.products = [product];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "test_prominent_alert") throw new Error("面板未显示");
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: `突出提醒${product.name}` }));
    const confirmation = await screen.findByRole("dialog", { name: "先体验突出提醒" });
    await fireEvent.click(within(confirmation).getByRole("button", { name: "测试" }));
    await screen.findByRole("alert");
    expect(screen.getByRole("dialog", { name: "先体验突出提醒" })).toBeTruthy();
    expect(current.products[0].prominentAlert).toBe(false);
    expect(localStorage.getItem("rm.prominent.tested")).toBeNull();
  });

  it("商品查找默认折叠，可展开编号查找及扫描再折叠保留输入", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect(screen.getByPlaceholderText('如"官翻品" "GR III"')).toBeTruthy();
    expect(screen.queryByLabelText("Product ID")).toBeNull();
    const toggle = screen.getByRole("button", { name: "查找商城商品" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await fireEvent.click(toggle);
    await fireEvent.input(screen.getByLabelText("Product ID"), { target: { value: "65" } });
    expect(screen.getByRole("button", { name: "开始扫描" })).toBeTruthy();
    await fireEvent.click(toggle);
    expect(screen.queryByLabelText("Product ID")).toBeNull();
    await fireEvent.click(toggle);
    expect((screen.getByLabelText("Product ID") as HTMLInputElement).value).toBe("65");
  });

  it("商品目录静默过滤 test 名称并省略检查状态", async () => {
    current.products = [product];
    current.catalog = [{ ...product, productId: "900", name: "GR TEST 样品", enabled: false }, { ...product, productId: "901", name: "test 相机", enabled: false }, { ...product, productId: "65", name: "GR III", observation: null, enabled: false }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect(screen.queryByText("GR TEST 样品")).toBeNull();
    expect(screen.queryByText("test 相机")).toBeNull();
    expect(screen.queryByText(/未上架|尚无成功检查|库存未提供/)).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("checkbox", { name: "GR III" })).toBeTruthy();
  });

  it.each([true, false])("删除监控状态为%s的商品，按启用状态决定二次确认", async (enabled) => {
    current.products = [{ ...product, enabled }];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query);
      if (name === "remove_product") { current.products = []; current.catalog = []; return { message: null }; }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    await fireEvent.click(screen.getByRole("button", { name: "查看GR IV HDF详情" }));
    const drawer = screen.getByRole("dialog", { name: "商品详情" });
    expect(within(drawer).queryByText(/商品图/)).toBeNull();
    await fireEvent.click(within(drawer).getByRole("button", { name: "删除商品" }));
    if (enabled) {
      expect(bridge.invoke.mock.calls.some(([name]) => name === "remove_product")).toBe(false);
      const confirmation = within(drawer).getByRole("group", { name: "确认删除GR IV HDF" });
      await fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
      expect(within(drawer).queryByRole("group", { name: "确认删除GR IV HDF" })).toBeNull();
      await fireEvent.click(within(drawer).getByRole("button", { name: "删除商品" }));
      await fireEvent.click(within(drawer).getByRole("button", { name: "确认删除" }));
    }
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("remove_product", { productId: "245" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "商品详情" })).toBeNull());
    expect(screen.queryByRole("checkbox", { name: "GR IV HDF" })).toBeNull();
  });

  it("最近消息显示到秒，悬停展示毫秒", async () => {
    current.recentChecks = [{ productId: "245", name: "GR IV HDF", availability: "out_of_stock", isShow: 0, stock: 0, at: "2026-10-06T10:00:00.123+08:00" }];
    render(Page);
    const table = await screen.findByRole("table");
    await waitFor(() => expect(table.querySelector(".message-clock")?.textContent).toBe("10:00:00"));
    expect(table.querySelector("tbody td")?.getAttribute("title")).toContain("10:00:00.123");
  });

  it("首次订阅快照早于读取结果时仍加载历史消息", async () => {
    current.runtime!.state = "paused";
    current.recentChecks = [{ productId: "245", name: "GR IV HDF", availability: "out_of_stock", isShow: 0, stock: 0, at: "2026-10-06T10:00:00+08:00" }];
    const pending = deferred<DesktopSnapshot>();
    bridge.invoke.mockImplementationOnce(() => pending.promise);
    render(Page);
    await waitFor(() => expect(bridge.eventHandler).toBeTruthy());
    bridge.eventHandler!({ payload: structuredClone(current) });
    await screen.findByRole("heading", { name: "已暂停" });
    pending.resolve(structuredClone(current));
    await pending.promise;
    expect(await screen.findByText("检查结果：未上架")).toBeTruthy();
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "query_messages")).toHaveLength(1);
  });

  it.each(["旧快照", "旧错误"])("切回窗口的%s迟到时保留已收到的新状态", async (result) => {
    current.products = [{ ...product, todayCheckCount: 708 }];
    current.runtime!.state = "paused";
    render(Page);
    await screen.findByRole("heading", { name: "已暂停" });
    const old = structuredClone(current);
    const pending = deferred<DesktopSnapshot>();
    bridge.invoke.mockImplementationOnce(() => pending.promise);
    await fireEvent(window, new Event("focus"));
    current.products[0].todayCheckCount = 800;
    current.runtime!.state = "monitoring";
    bridge.eventHandler!({ payload: structuredClone(current) });
    await screen.findByRole("heading", { name: "正在监控" });
    if (result === "旧快照") pending.resolve(old);
    else pending.reject(new Error("旧读取连接失败"));
    await pending.promise.catch(() => {});
    await new Promise((resolve) => setTimeout(resolve));
    expect(screen.getByRole("heading", { name: "正在监控" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "今日检查 800 次，查看记录" })).toBeTruthy();
    expect(screen.queryByText("旧读取连接失败")).toBeNull();
  });

  it("再次浏览商品页会请求补齐缺失资料，允许暂时失败后重试", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await waitFor(() => expect(bridge.refreshCatalog).toHaveBeenCalled());
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    const firstVisit = bridge.refreshCatalog.mock.calls.length;
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect(bridge.refreshCatalog.mock.calls.length).toBe(firstVisit + 1);
  });
  it("最近消息可折叠和恢复，分隔条支持键盘调宽并保存偏好", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    const splitter = screen.getByRole("separator", { name: "调整最近消息宽度" });
    await fireEvent.keyDown(splitter, { key: "ArrowLeft" });
    expect(Number(splitter.getAttribute("aria-valuenow"))).toBeGreaterThan(320);
    expect(localStorage.getItem("rm.messages.width")).not.toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "折叠最近消息" }));
    expect(screen.getByRole("button", { name: "展开最近消息" })).toBeTruthy();
    expect(screen.queryByRole("separator", { name: "调整最近消息宽度" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "展开最近消息" }));
    expect(screen.getByRole("separator", { name: "调整最近消息宽度" })).toBeTruthy();
  });
  it("恢复过宽偏好后，首次键盘缩窄从当前可用宽度开始", async () => {
    localStorage.setItem("rm.messages.width", "900");
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    const splitter = screen.getByRole("separator", { name: "调整最近消息宽度" });
    const before = Number(splitter.getAttribute("aria-valuenow"));
    await fireEvent.keyDown(splitter, { key: "ArrowRight" });
    expect(Number(splitter.getAttribute("aria-valuenow"))).toBe(before - 24);
  });
  it("帮助引导可取消，退出不写入商品设置", async () => {
    current.products = [product];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "帮助引导" }));
    await fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.getByRole("button", { name: "状态" })).toBeTruthy();
    expect(bridge.invoke.mock.calls.some(([name]) => name === "set_onboarding_products")).toBe(false);
  });
  it("帮助引导直接展示已选商品，进入引导不改变配置", async () => {
    current.products = [{ ...product, productId: "9", name: "RICOH GR IIIx" }];
    current.catalog = structuredClone(onboardingCatalog.filter((item) => item.productId !== "9"));
    current.config!.schedule = { days: ["mon", "wed"], start: "10:30", end: "18:00" };
    const schedule = structuredClone(current.config!.schedule);
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "帮助引导" }));
    await screen.findByRole("heading", { name: /选择监控商品/ });
    expect((screen.getByRole("checkbox", { name: "RICOH GR IIIx" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "官翻品 GR IV" }) as HTMLInputElement).checked).toBe(false);
    expect(screen.queryByRole("button", { name: "退出引导" })).toBeNull();
    expect(current.config!.schedule).toEqual(schedule);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "restore_defaults" || name === "save_monitoring_config" || name === "complete_setup")).toBe(false);
  });

  it("帮助引导再次完成时沿用已有计划与停止状态", async () => {
    current.products = [product];
    current.catalog = structuredClone(onboardingCatalog.filter((item) => item.productId !== "245"));
    current.config!.schedule = { days: ["fri"], start: "10:30", end: "18:00" };
    const schedule = structuredClone(current.config!.schedule);
    useOnboardingBridge();
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "帮助引导" }));
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    await screen.findByRole("heading", { name: /恭喜！/ });
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    await screen.findByRole("heading", { name: "已停止" });
    expect(current.config!.schedule).toEqual(schedule);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "complete_setup")).toBe(false);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "restore_defaults")).toBe(false);
  });

  it("已选商品可启用突出提醒并保存该商品开关", async () => {
    localStorage.setItem("rm.prominent.tested", "true");
    current.products = [product];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "set_product_prominent_alert") {
        current.products = current.products.map((item) => ({ ...item, prominentAlert: args!.enabled }));
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    const toggle = screen.getByRole("button", { name: "突出提醒GR IV HDF" });
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    await fireEvent.click(toggle);
    await waitFor(() => expect(current.products[0].prominentAlert).toBe(true));
    expect(bridge.invoke).toHaveBeenCalledWith("set_product_prominent_alert", { productId: "245", enabled: true });
  });

  it.each(["products", "catalog"] as const)("通知页直接测试突出提醒，无需选商品或启用开关（%s）", async (list) => {
    current[list] = [
      { ...product, enabled: list === "products" },
      { ...product, productId: "130", name: "官翻品 GR IV", enabled: list === "products" },
    ];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "test_prominent_alert") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.queryByRole("combobox", { name: "测试商品" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "测试突出提醒" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("test_prominent_alert", { productId: list === "products" ? "245" : "130" }));
    expect(current[list].every((item) => !item.prominentAlert)).toBe(true);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "set_product_prominent_alert" || name === "set_system_notifications_enabled")).toBe(false);
  });

  it("其他设置切换监控方案，速率页显示有效模式，导入保留模式", async () => {
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_monitoring_config") {
        current.config = structuredClone(args?.config);
        return { message: null };
      }
      if (name === "import_configuration_file") return "/tmp/config.json";
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect(screen.queryByRole("combobox", { name: "监控方案" })).toBeNull();
    expect(screen.getByText(/每轮列表检查间隔/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    const mode = screen.getByRole("radio", { name: "全站上架列表（推荐）" }) as HTMLInputElement;
    expect(mode.checked).toBe(true);
    expect(screen.getByText(/一轮分页检查.*所有监控商品/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("radio", { name: "逐商品详情" }));
    expect(screen.getByText(/逐商品请求详情/)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect(current.config?.monitoringMode).toBe("product_detail"));
    expect(bridge.invoke).toHaveBeenCalledWith("save_monitoring_config", { config: expect.objectContaining({ monitoringMode: "product_detail" }) });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect(screen.getByText(/每个监控商品请求间隔/)).toBeTruthy();
    current.config!.monitoringMode = "listed_products";
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "导入配置…" }));
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await waitFor(() => expect((screen.getByRole("radio", { name: "全站上架列表（推荐）" }) as HTMLInputElement).checked).toBe(true));
  });

  it("两种监控方案以单选卡片呈现优缺点", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect(screen.queryByRole("combobox", { name: "监控方案" })).toBeNull();
    expect(screen.getAllByRole("radio")).toHaveLength(2);
    expect(screen.getAllByText("优点")).toHaveLength(2);
    expect(screen.getAllByText("缺点")).toHaveLength(2);
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
  });

  it("完整列表中缺席的商品显示未上架，空库存不显示提示或零库存", async () => {
    current.products = [{ ...product, observation: { availability: "out_of_stock", isShow: 0, stock: null, checkedAt: "2026-10-06T10:00:00+08:00" } }];
    current.recentChecks = [{ productId: "245", name: "GR IV HDF", availability: "out_of_stock", isShow: 0, stock: null, at: "2026-10-06T10:00:00+08:00" }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    const tile = screen.getByRole("button", { name: "查看GR IV HDF详情" }).closest(".product-tile")!;
    expect(within(tile as HTMLElement).getByText("未上架")).toBeTruthy();
    expect(within(tile as HTMLElement).queryByText("库存未提供")).toBeNull();
    expect(screen.queryByText("无货 0")).toBeNull();
    const row = (await screen.findByRole("table")).querySelector("tbody tr")!;
    expect(row.children[3]?.textContent).toBe("—");
    await fireEvent.click(screen.getByRole("button", { name: "查看GR IV HDF详情" }));
    expect(within(screen.getByRole("dialog", { name: "商品详情" })).getByText("未上架")).toBeTruthy();
    expect(within(screen.getByRole("dialog", { name: "商品详情" })).queryByText(/库存未提供/)).toBeNull();
  });
  it("商品照片被固定容器裁定尺寸，价格和今日计数留在正文", async () => {
    current.products = [{ ...product, imagePath: "/tmp/245.jpg", metadata: { imageUrl: null, galleryUrls: [], price: "8819.0", unitName: "台", productNo: "B1555", isMemberCard: false, memberCardDays: null }, todayCheckCount: 12 }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    expect(tileSource).toMatch(/\.product-photo \{[^}]*position: relative;[^}]*height: 134px;[^}]*overflow: hidden/);
    expect(tileSource).toMatch(/\.product-photo img \{[^}]*position: absolute;[^}]*object-fit: contain/);
    expect(tileSource).toMatch(/\.product-popover \{[^}]*left: 12px; right: 12px/);
    expect(screen.getByText("¥8819.0 / 台")).toBeTruthy();
    expect(screen.getByRole("button", { name: "今日检查 12 次，查看记录" }).textContent).toContain("12");
    expect(screen.getByRole("button", { name: "今日检查 12 次，查看记录" }).textContent).not.toMatch(/今日检查|次/);
    expect(screen.queryByText("检查 37 次")).toBeNull();
  });
  it("状态页以真实商品图展示监控商品，照片打开详情并读取该商品消息", async () => {
    current.products = [{ ...product, imagePath: "/tmp/ricoh-245.jpg", metadata: null, metadataUpdatedAt: null, todayCheckCount: 12, todaySuccessCount: 11, todayFailureCount: 1, monitoringMs: 3_661_000 } as ProductRecord];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    const image = screen.getByRole("img", { name: "GR IV HDF" });
    expect(image.closest(".product-tile")?.querySelector(".product-popover")).toBeTruthy();
    expect(image.getAttribute("src")).toContain("ricoh-245.jpg");
    await fireEvent.click(image);
    expect(await screen.findByRole("dialog", { name: "商品详情" })).toBeTruthy();
    expect(bridge.invoke.mock.calls.some(([name, args]) => name === "query_messages" && args?.query?.productId === "245")).toBe(true);
    expect(within(screen.getByRole("dialog", { name: "商品详情" })).getByText("今日检查 12 次")).toBeTruthy();
  });

  it("详情记录原样显示核心层消息，不重复追加库存", async () => {
    current.products = [product];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return { items: [{ at: "2026-10-06T10:00:00+08:00", productId: "245", name: product.name, isShow: 1, stock: 5, detail: "补货 3 → 5" }], nextCursor: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "查看GR IV HDF详情" }));
    const drawer = await screen.findByRole("dialog", { name: "商品详情" });
    await waitFor(() => expect(drawer.querySelector(".drawer-records span")?.textContent).toBe("补货 3 → 5"));
    expect(tileSource).toMatch(/\.product-popover \{[^}]*pointer-events: none/);
  });

  it("商品列表省略错误内容，详情保留原始错误并可用 Escape 关闭", async () => {
    current.products = [{ ...product, runtimeError: "HTTP 503: 原始响应" }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    await fireEvent.click(screen.getByRole("button", { name: "查看GR IV HDF详情" }));
    expect(screen.getByRole("dialog", { name: "商品详情" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "查看GR IV HDF详情" }).closest(".product-option")).toBeTruthy();
    const drawer = screen.getByRole("dialog", { name: "商品详情" });
    expect(within(drawer).getByText("HTTP 503: 原始响应").tagName).toBe("PRE");
    expect(within(drawer).getByRole("button", { name: "商品详情突出提醒" }).textContent).toContain("未开启");
    expect(within(drawer).queryByRole("button", { name: /查看全部记录/ })).toBeNull();
    expect(pageSource).toMatch(/\.product-detail-drawer \{[^}]*width: 394px/);
    await fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "商品详情" })).toBeNull();
    expect(screen.queryByText("HTTP 503: 原始响应")).toBeNull();
  });
  it("侧栏图标循环切换系统、浅色、深色，并同步外观设置", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "外观：跟随系统" }));
    expect(document.documentElement.dataset.theme).toBe("light");
    await fireEvent.click(screen.getByRole("button", { name: "外观：浅色" }));
    expect(document.documentElement.dataset.theme).toBe("dark");
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect((screen.getByLabelText("外观") as HTMLSelectElement).value).toBe("dark");
    await fireEvent.click(screen.getByRole("button", { name: "外观：深色" }));
    expect(localStorage.getItem("rm.theme")).toBe("system");
    expect((screen.getByLabelText("外观") as HTMLSelectElement).value).toBe("system");
  });

  it("渠道测试待完成时，同一渠道不会重复发送，切页后仍可完成", async () => {
    current.channels = [channel];
    const pending = deferred<{ outcome: "accepted"; message: string }>();
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "test_notification_channel") return pending.promise;
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    const testButton = screen.getByRole("button", { name: "测试工作群" });
    await fireEvent.click(testButton);
    await fireEvent.click(testButton);
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "test_notification_channel")).toHaveLength(1);
    expect(testButton.hasAttribute("disabled")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    pending.resolve({ outcome: "accepted", message: "平台已接受" });
    expect((await screen.findByRole("status")).textContent).toContain("测试成功");
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.getByRole("button", { name: "测试工作群" }).hasAttribute("disabled")).toBe(false);
  });

  it("同一商品切换待完成时锁住该行，失败后回滚", async () => {
    current.products = [product];
    const pending = deferred<{ message: null }>();
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "set_product_enabled") return pending.promise;
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    const toggle = screen.getByRole("checkbox", { name: "GR IV HDF" }) as HTMLInputElement;
    await fireEvent.click(toggle);
    await fireEvent.click(toggle);
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "set_product_enabled")).toHaveLength(1);
    expect(toggle.disabled).toBe(true);
    pending.reject(new Error("保存失败"));
    expect((await screen.findByRole("alert")).textContent).toContain("保存失败");
    expect(toggle.checked).toBe(true);
  });

  it("并行操作结束一个时，另一操作仍保持忙碌状态", async () => {
    current.channels = [channel];
    const pending = deferred<{ outcome: "accepted"; message: string }>();
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "test_notification_channel") return pending.promise;
      if (name === "set_system_notifications_enabled") throw new Error("保存失败");
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "测试工作群" }));
    const toggle = screen.getByRole("checkbox", { name: "启用系统通知" }) as HTMLInputElement;
    await fireEvent.click(toggle);
    expect((await screen.findByRole("alert")).textContent).toContain("保存失败");
    expect(toggle.checked).toBe(false);
    expect(screen.getByRole("button", { name: "发送测试" }).hasAttribute("disabled")).toBe(true);
    pending.resolve({ outcome: "accepted", message: "平台已接受" });
    await waitFor(() => expect(screen.getByRole("button", { name: "发送测试" }).hasAttribute("disabled")).toBe(false));
  });
  it("六页导航保留品牌字标，状态页使用完整宽度的消息表", async () => {
    render(Page);
    await screen.findByRole("heading", { name: "已停止" });
    expect(screen.getByRole("img", { name: "RM" }).getAttribute("src")).toBe("/brand/rm-wordmark.svg");
    expect(screen.getByText("RichoMonitor")).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "主导航" }).querySelectorAll("button")).toHaveLength(6);
    expect(pageSource).toMatch(/\.message-table \{[^}]*min-width: 560px/);
    expect(pageSource).toMatch(/\.shell \{[^}]*grid-template-columns: 210px/);
  });

  it("外观默认跟随系统，切换仅保存界面偏好并跨页面保留", async () => {
    render(Page);
    await screen.findByRole("heading", { name: "已停止" });
    expect(document.documentElement.dataset.theme).toBe("system");
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    const appearance = screen.getByRole("combobox", { name: "外观" }) as HTMLSelectElement;
    expect(appearance.value).toBe("system");
    for (const preference of ["light", "dark", "system"]) {
      await fireEvent.change(appearance, { target: { value: preference } });
      expect(document.documentElement.dataset.theme).toBe(preference);
      expect(localStorage.getItem("rm.theme")).toBe(preference);
    }
    await fireEvent.change(appearance, { target: { value: "light" } });
    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect((screen.getByRole("combobox", { name: "外观" }) as HTMLSelectElement).value).toBe("light");
    expect(bridge.invoke.mock.calls.every(([name]) => name === "get_desktop_snapshot" || name === "query_messages")).toBe(true);
  });

  it("重新打开界面沿用上次选择的主题", async () => {
    localStorage.setItem("rm.theme", "dark");
    render(Page);
    await screen.findByRole("heading", { name: "已停止" });
    expect(document.documentElement.dataset.theme).toBe("dark");
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect((screen.getByRole("combobox", { name: "外观" }) as HTMLSelectElement).value).toBe("dark");
  });

  it("切换页面回到主栏顶部，同页更新保留滚动位置", async () => {
    render(Page);
    await screen.findByRole("heading", { name: "已停止" });
    const main = screen.getByRole("main");

    main.scrollTop = 240;
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect(main.scrollTop).toBe(0);
    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    expect(main.scrollTop).toBe(0);

    main.scrollTop = 180;
    bridge.eventHandler?.({ payload: makeSnapshot({
      runtime: { state: "monitoring", nextStartAt: null, lastSuccessAt: null, lastError: null },
    }) });
    await screen.findByRole("heading", { name: "正在监控" });
    expect(main.scrollTop).toBe(180);

    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    expect(main.scrollTop).toBe(180);

    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(main.scrollTop).toBe(0);
  });

  it("窄屏页面切换回到窗口顶部", async () => {
    vi.stubGlobal("innerWidth", 600);
    const scrollTo = vi.spyOn(window, "scrollTo").mockImplementation(() => {});
    render(Page);
    await screen.findByRole("heading", { name: "已停止" });
    scrollTo.mockClear();

    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect(scrollTo).toHaveBeenCalledWith(0, 0);
  });

  it("六页导航独立于主栏，并准确标示当前页面", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    const nav = screen.getByRole("navigation", { name: "主导航" });
    expect(nav.closest("aside")).toBe(screen.getByRole("complementary"));
    expect(screen.getByRole("main").contains(nav)).toBe(false);
    expect(nav.querySelectorAll("button")).toHaveLength(6);
    expect(screen.getByRole("button", { name: "状态" }).getAttribute("aria-current")).toBe("page");
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect(screen.getByRole("button", { name: "速率" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("button", { name: "状态" }).hasAttribute("aria-current")).toBe(false);
    expect(screen.getByRole("group", { name: "选择星期" }).querySelectorAll("input")).toHaveLength(7);
    await fireEvent.click(screen.getByRole("checkbox", { name: "周一" }));
    expect((screen.getByRole("checkbox", { name: "周一" }) as HTMLInputElement).checked).toBe(false);
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "重置更改" }));
    expect((screen.getByRole("checkbox", { name: "周一" }) as HTMLInputElement).checked).toBe(true);
  });

  it("正式页面不重复显示品牌或全局页面标题", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    expect(screen.queryByText("理光库存监控")).toBeNull();
    for (const name of ["通知", "速率", "代理池", "监控产品", "其他设置"]) {
      await fireEvent.click(screen.getByRole("button", { name }));
      expect(screen.queryByRole("heading", { level: 1 })).toBeNull();
    }
  });

  it("浏览器用样本数据预览导航及暂停操作，不调用监控后台", async () => {
    bridge.isTauri.mockReturnValue(false);
    render(Page);
    await screen.findByText("界面预览 · 不连接监控");
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect(screen.getByText("暂无代理")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    await fireEvent.click(screen.getByRole("button", { name: "暂停" }));
    expect(await screen.findByRole("heading", { name: "已暂停" })).toBeTruthy();
    expect(bridge.invoke).not.toHaveBeenCalledWith("monitoring_action", expect.anything());
  });

  it("首次引导默认选择全部官翻 GR，按下一步原子保存后设置通知并完成", async () => {
    current.setupCompleted = false;
    current.catalog = structuredClone(onboardingCatalog);
    useOnboardingBridge();

    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    for (const item of onboardingCatalog.filter((item) => item.productId !== "9")) {
      expect((screen.getByRole("checkbox", { name: item.name }) as HTMLInputElement).checked).toBe(item.name.startsWith("官翻品"));
    }
    expect(bridge.invoke.mock.calls.every(([name]) => name === "get_desktop_snapshot" || name === "query_messages")).toBe(true);
    expect(screen.queryByRole("navigation", { name: "主导航" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    expect(bridge.invoke).toHaveBeenCalledWith("set_onboarding_products", { productIds: ["19", "65", "66", "130", "245"] });
    expect(bridge.invoke.mock.calls.some(([name]) => ["add_product", "validate_product", "set_product_enabled", "monitoring_action"].includes(name))).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "启用系统通知" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("set_system_notifications_enabled", { enabled: true }));
    expect(bridge.invoke).toHaveBeenCalledWith("request_notification_permission", undefined);
    expect(current.platform!.notificationPermission).toBe("granted");
    expect(current.systemNotificationsEnabled).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /恭喜！/ });
    expect(current.setupCompleted).toBe(false);
    await fireEvent.click(screen.getByRole("checkbox", { name: "登录后启动" }));
    expect(bridge.invoke.mock.calls.some(([name]) => name === "set_login_start")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(await screen.findByRole("heading", { name: "正在监控" })).toBeTruthy();
    expect(bridge.invoke).toHaveBeenCalledWith("set_login_start", { enabled: true });
    expect(bridge.invoke).toHaveBeenCalledWith("save_monitoring_config", { config: expect.objectContaining({ schedule: { days: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"], start: "00:00", end: "00:00" } }) });
    expect(bridge.invoke).toHaveBeenCalledWith("complete_setup", undefined);
    expect(current.setupCompleted).toBe(true);
  });

  it("引导通知页发送系统测试后可继续完成设置", async () => {
    current.setupCompleted = false;
    current.products = [product];
    current.catalog = structuredClone(onboardingCatalog);
    current.systemNotificationsEnabled = true;
    current.platform!.notificationPermission = "prompt";
    useOnboardingBridge();

    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "测试系统通知" }));
    await screen.findByText("系统已接受测试通知。");
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "test_system_notification")).toHaveLength(1);
    expect(bridge.invoke).not.toHaveBeenCalledWith("request_notification_permission", undefined);
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /恭喜！/ });
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(await screen.findByRole("heading", { name: "正在监控" })).toBeTruthy();
  });

  it("引导已有快照时刷新失败保留选择与操作，重试后可继续", async () => {
    current.setupCompleted = false;
    current.catalog = structuredClone(onboardingCatalog);
    useOnboardingBridge();
    const invoke = bridge.invoke.getMockImplementation()!;
    let refreshFailed = false;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot" && refreshFailed) throw new Error("状态读取暂时失败");
      return invoke(name, args);
    });
    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("checkbox", { name: "GR SPACE VIP会员卡" }));

    refreshFailed = true;
    await fireEvent.focus(window);
    expect((await screen.findByRole("alert")).textContent).toContain("状态读取暂时失败");
    expect((screen.getByRole("checkbox", { name: "GR SPACE VIP会员卡" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("button", { name: "下一步" }) as HTMLButtonElement).disabled).toBe(false);

    refreshFailed = false;
    await fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    expect(bridge.invoke).toHaveBeenCalledWith("set_onboarding_products", { productIds: ["19", "65", "66", "130", "245", "108"] });
  });

  it("系统仍未授权时不发送测试通知", async () => {
    current.platform!.notificationPermission = "denied";
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "test_system_notification") return "permission_denied";
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "发送测试" }));
    expect((await screen.findByRole("alert")).textContent).toContain("系统通知权限未允许");
    expect(bridge.invoke).toHaveBeenCalledWith("test_system_notification", undefined);
    expect(bridge.invoke).not.toHaveBeenCalledWith("request_notification_permission", undefined);
  });

  it.each(["denied", "granted"] as const)("系统通知操作失败后同步 %s 权限并保留实际错误", async (permission) => {
    current.platform!.notificationPermission = "prompt";
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "test_system_notification") {
        current.platform!.notificationPermission = permission;
        if (permission === "denied") return "permission_denied";
        throw "系统通知发送失败：通知服务暂不可用";
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "发送测试" }));
    if (permission === "denied") expect(await screen.findByText("权限：已关闭")).toBeTruthy();
    else await screen.findByText("权限：已允许");
    expect((await screen.findByRole("alert")).textContent).toContain(permission === "denied" ? "系统通知权限未允许" : "系统通知发送失败：通知服务暂不可用");
  });

  it("停用监控时权限推送立即更新界面，无需切回窗口", async () => {
    current.platform!.notificationPermission = "granted";
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await screen.findByText("权限：已允许");
    await waitFor(() => expect(bridge.eventHandler).not.toBeNull());
    for (const permission of ["denied", "granted"] as const) {
      current.platform!.notificationPermission = permission;
      bridge.eventHandler!({ payload: structuredClone(current) });
      await screen.findByText(permission === "denied" ? "权限：已关闭" : "权限：已允许");
    }
    expect(bridge.invoke).not.toHaveBeenCalledWith("refresh_notification_permission", undefined);
  });

  it("窗口重新获得焦点时读取外部修改的系统权限", async () => {
    current.platform!.notificationPermission = "denied";
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.getByText("权限：已关闭")).toBeTruthy();
    current.platform!.notificationPermission = "granted";
    await fireEvent.focus(window);
    await screen.findByText("权限：已允许");
  });

  it("通知权限读取失败保持一处提示，普通状态推送不清除，真实恢复才清除", async () => {
    const failure = "Windows 通知权限读取失败，请重新检查：找不到元素。(0x80070490)";
    current.systemNotificationsEnabled = true;
    current.platform!.notificationPermission = "unavailable";
    current.platform!.notificationPermissionError = failure;
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert").textContent).toContain(failure);
    expect(screen.getByText("权限：暂时无法读取")).toBeTruthy();
    expect(screen.queryByText("权限：首次发送时询问")).toBeNull();
    await waitFor(() => expect(bridge.eventHandler).not.toBeNull());
    bridge.eventHandler!({ payload: structuredClone(current) });
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain(failure));
    current.platform!.notificationPermission = "granted";
    current.platform!.notificationPermissionError = null;
    bridge.eventHandler!({ payload: structuredClone(current) });
    await screen.findByText("权限：已允许");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("手动检查权限失败时不同时显示同一个错误横幅和浮动提示", async () => {
    const failure = "Windows 通知权限读取失败，请重新检查：找不到元素。(0x80070490)";
    current.systemNotificationsEnabled = true;
    current.platform!.notificationPermission = "unavailable";
    current.platform!.notificationPermissionError = failure;
    const invoke = bridge.invoke.getMockImplementation()!;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "refresh_notification_permission") throw failure;
      return invoke(name, args);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "检查权限" }));
    await waitFor(() => {
      expect(screen.getAllByRole("alert")).toHaveLength(1);
      expect(screen.getByRole("alert").textContent).toContain(failure);
    });
    current.platform!.notificationPermission = "granted";
    current.platform!.notificationPermissionError = null;
    bridge.eventHandler!({ payload: structuredClone(current) });
    await screen.findByText("权限：已允许");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("手动检查首次读取失败、最终读取恢复后不再弹出已过时的错误", async () => {
    const failure = "Windows 通知权限读取失败，请重新检查：找不到元素。(0x80070490)";
    current.systemNotificationsEnabled = true;
    current.platform!.notificationPermission = "unavailable";
    current.platform!.notificationPermissionError = failure;
    const invoke = bridge.invoke.getMockImplementation()!;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "refresh_notification_permission") {
        current.platform!.notificationPermission = "granted";
        current.platform!.notificationPermissionError = null;
        throw failure;
      }
      return invoke(name, args);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "检查权限" }));
    await screen.findByText("权限：已允许");
    await waitFor(() => {
      expect((screen.getByRole("button", { name: "检查权限" }) as HTMLButtonElement).disabled).toBe(false);
      expect(screen.queryByRole("alert")).toBeNull();
    });
  });

  it("首次引导在完成前保持未完成状态，重开界面仍进入商品选择", async () => {
    current.setupCompleted = false;
    current.systemNotificationsEnabled = true;
    current.catalog = structuredClone(onboardingCatalog);
    useOnboardingBridge();

    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    expect(current.setupCompleted).toBe(false);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "complete_setup")).toBe(false);
    cleanup();
    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    await screen.findByRole("heading", { name: /恭喜！/ });
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(await screen.findByRole("heading", { name: "正在监控" })).toBeTruthy();
    expect(bridge.invoke).toHaveBeenCalledWith("complete_setup", undefined);
    expect(current.setupCompleted).toBe(true);
  });

  it("已有商品时无需启用通知也可完成引导", async () => {
    current.setupCompleted = false;
    current.products = [product];
    current.catalog = structuredClone(onboardingCatalog);
    current.systemNotificationsEnabled = false;
    useOnboardingBridge();
    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    await screen.findByRole("heading", { name: /恭喜！/ });
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(await screen.findByRole("heading", { name: "正在监控" })).toBeTruthy();
    expect(bridge.invoke).toHaveBeenCalledWith("complete_setup", undefined);
    expect(current.systemNotificationsEnabled).toBe(false);
    expect(bridge.invoke.mock.calls.some(([name]) => name === "request_notification_permission" || name === "set_system_notifications_enabled")).toBe(false);
  });

  it("状态页显示库存和异常，并按状态提供暂停操作", async () => {
    current.runtime!.state = "monitoring";
    current.runtime!.lastError = "部分商品暂时无法连接";
    const rawError = "请求失败\n连接超时：详情接口";
    current.products = [{ ...product, runtimeError: rawError, observation: { availability: "in_stock", isShow: 1, stock: 2, checkedAt: "2026-09-28T00:15:00Z" } }];
    current.recentEvents = [{ id: 1, at: "2026-09-28T00:15:00Z", kind: "error", productId: "245", message: "连接失败" }];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "monitoring_action") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    const globalError = await screen.findByRole("button", { name: "监控异常" });
    expect(screen.queryByText("尚无成功检查")).toBeNull();
    expect(screen.queryByText("部分商品暂时无法连接")).toBeNull();
    await fireEvent.click(globalError);
    expect(screen.getByRole("dialog", { name: "监控异常" }).querySelector("pre")?.textContent).toBe("部分商品暂时无法连接");
    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    const failed = screen.getByRole("button", { name: "检查失败" });
    expect(failed.classList.contains("product-error-link")).toBe(true);
    expect(screen.queryByText("有货（2）")).toBeNull();
    expect(screen.getByText((_, element) => element?.tagName === "PRE" && element.textContent === rawError)).toBeTruthy();
    await fireEvent.click(failed);
    expect(screen.getByRole("dialog", { name: "检查失败" }).querySelector("pre")?.textContent).toBe(rawError);
    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog", { name: "检查失败" })).toBeNull();
    expect(screen.queryByText("未知")).toBeNull();
    expect(screen.queryByRole("link", { name: "通知渠道（0）" })).toBeNull();
    expect(screen.queryByRole("link", { name: "代理池（0）" })).toBeNull();
    expect(screen.getByText("累计检查 37 次")).toBeTruthy();
    expect(screen.getByTitle("最近成功检查").textContent).toContain("9/28 08:15:00.000");
    expect(screen.queryByRole("checkbox", { name: "GR IV HDF" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "暂停" }));
    expect(bridge.invoke).toHaveBeenCalledWith("monitoring_action", { action: "pause" });
  });

  it("全局错误与商品错误相同时只保留商品入口", async () => {
    current.runtime!.lastError = "详情接口请求失败";
    current.products = [{ ...product, runtimeError: "详情接口请求失败" }];
    render(Page);
    await screen.findByRole("button", { name: "检查失败" });
    expect(screen.queryByRole("button", { name: "监控异常" })).toBeNull();
    expect(screen.getByText("详情接口请求失败")).toBeTruthy();
  });

  it("检查失败时仅显示错误入口，不显示失效的上架状态", async () => {
    current.products = [{ ...product, runtimeError: "请求超时" }];
    render(Page);
    await screen.findByRole("button", { name: "检查失败" });
    expect(screen.queryByText("未知")).toBeNull();
    expect(screen.queryByText("未上架")).toBeNull();
  });

  it("顶部不再显示最近成功检查时间", async () => {
    current.runtime!.lastSuccessAt = "2026-09-28T00:15:00Z";
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    expect(screen.queryByText("最近成功检查 9/28 08:15:00.000")).toBeNull();
  });

  it("尚未成功检查时显示明确状态", async () => {
    current.products = [{ ...product, observation: null }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    expect(screen.getByText("尚无成功检查")).toBeTruthy();
    expect(screen.queryByText("等待首次检查")).toBeNull();
  });

  it("监控任务退出时显示历史结果和诊断，并阻止无效启动", async () => {
    current.runtime = { state: "worker_failed", nextStartAt: null, lastSuccessAt: "2026-09-28T00:15:00Z", lastError: "任务意外退出" };
    render(Page);
    await screen.findByRole("heading", { name: "监控任务已停止" });
    expect(screen.getByText("以下为历史检查结果。重新打开应用后可继续监控。")).toBeTruthy();
    expect(screen.getByRole("button", { name: "重新打开应用" }).hasAttribute("disabled")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "监控异常" }));
    expect(screen.getByText("任务意外退出")).toBeTruthy();
  });

  it("设置页显示产品名和打包版本", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect(screen.getByText("版本 0.1.0")).toBeTruthy();
    expect(screen.getAllByText("RichoMonitor").length).toBeGreaterThan(0);
  });

  it("成功检查后设置页显示暂未发现更新", async () => {
    render(Page);
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect(await screen.findByText("暂未发现更新。")).toBeTruthy();
  });

  it("主页更新事件同步到设置页，安装防重并在退出时释放订阅", async () => {
    render(Page);
    await waitFor(() => expect(bridge.updateHandler).toBeTruthy());
    const available: UpdateStatus = { phase: "available", version: "0.2.0", notes: "修复连接", downloaded: 0, total: null, error: null };
    bridge.updateHandler!({ payload: available });
    await waitFor(() => expect(screen.getByText("发现新版本 0.2.0")).toBeTruthy());
    expect(screen.queryByRole("alert", { name: /更新/ })).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    expect(screen.getAllByText(/发现新版本 0\.2\.0/).length).toBeGreaterThan(0);
    const install = screen.getAllByRole("button", { name: "下载并重启" })[0];
    await fireEvent.click(install);
    await fireEvent.click(install);
    expect(bridge.installUpdateCount).toBe(1);

    cleanup();
    expect(bridge.updateUnlisten).toHaveBeenCalledOnce();
  });

  it("安装失败保留可重试版本并直接显示原生错误", async () => {
    render(Page);
    await waitFor(() => expect(bridge.updateHandler).toBeTruthy());
    bridge.updateHandler!({ payload: { phase: "available", version: "0.2.0", notes: null, downloaded: 0, total: null, error: null } });
    bridge.installUpdatePromise = Promise.reject(new Error("更新失败：签名校验失败"));
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getAllByRole("button", { name: "下载并重启" })[0]);
    expect((await screen.findByRole("alert")).textContent).toBe("更新失败：签名校验失败");
    expect(screen.getByText(/发现新版本 0\.2\.0/)).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "下载并重启" })).toHaveLength(1);
  });

  it("安装期间到达较新原生事件时，失败回调不覆盖该状态", async () => {
    const pendingInstall = deferred<void>();
    bridge.installUpdatePromise = pendingInstall.promise;
    render(Page);
    await waitFor(() => expect(bridge.updateHandler).toBeTruthy());
    bridge.updateHandler!({ payload: { phase: "available", version: "0.2.0", notes: null, downloaded: 0, total: null, error: null } });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getAllByRole("button", { name: "下载并重启" })[0]);
    bridge.updateHandler!({ payload: { phase: "downloading", version: "0.2.0", notes: null, downloaded: 512, total: 1024, error: null } });
    pendingInstall.reject(new Error("旧请求失败"));
    await screen.findByText(/正在下载更新：50%/);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("大检查次数完整保留、无 info 图标，铃铛在上架状态前", async () => {
    current.products = [{ ...product, todayCheckCount: 999_999_999_999 }];
    render(Page);
    const counter = await screen.findByRole("button", { name: "今日检查 999999999999 次，查看记录" });
    expect(counter.querySelector("svg")).toBeNull();
    const tile = counter.closest("article")!;
    const bell = within(tile).getByRole("button", { name: `突出提醒${product.name}` });
    expect(bell.closest(".product-availability")).toBeTruthy();
    expect(bell.compareDocumentPosition(within(tile).getByText("未上架")) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    await fireEvent.click(counter);
    expect(await screen.findByRole("dialog", { name: "商品详情" })).toBeTruthy();
  });

  it("计划外显示北京时间下次开始，并允许暂停等待", async () => {
    current.runtime = { state: "outside_schedule", nextStartAt: "2026-09-30T09:00:00+08:00", lastSuccessAt: null, lastError: null };
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "monitoring_action") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    expect(await screen.findByRole("heading", { name: "等待计划开始" })).toBeTruthy();
    expect(screen.getByText("北京时间下次开始：9/30 09:00")).toBeTruthy();
    expect(screen.getByText("北京时间下次开始：9/30 09:00").parentElement).toBe(screen.getByRole("heading", { name: "等待计划开始" }).parentElement);
    expect(within(document.querySelector(".sidebar-state")!).getByText("等待")).toBeTruthy();
    expect(screen.queryByText("尚无成功检查")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "暂停" }));
    expect(bridge.invoke).toHaveBeenCalledWith("monitoring_action", { action: "pause" });
  });

  it("状态页将状态变化列为消息，并可按日期和商品筛选", async () => {
    current.recentChecks = [
      { productId: "245", name: "GR IV HDF", availability: "out_of_stock", isShow: 0, stock: 0, at: "2026-09-29T00:00:00.123Z" },
      { productId: "130", name: "GR IV", availability: "in_stock", isShow: 1, stock: 2, at: "2026-09-28T00:00:00.456Z" },
    ];
    current.recentEvents = [{ id: 1, at: "2026-09-28T00:00:00.456Z", kind: "stock_available", productId: "130", message: "商品 130 有货" }];
    render(Page);
    await screen.findByRole("heading", { name: /最近消息/ });
    expect(screen.getByRole("button", { name: "按日期筛选" }).textContent?.trim()).toBe("");
    expect(screen.getByRole("button", { name: "按商品筛选" }).textContent?.trim()).toBe("");
    expect(await screen.findByRole("table")).toBeTruthy();
    expect(screen.getByRole("table").querySelectorAll("tbody tr")).toHaveLength(2);
    expect(screen.getByText("9/29")).toBeTruthy();
    expect(screen.getAllByText("08:00:00")).toHaveLength(2);
    expect(screen.getByRole("table").querySelector("tbody td")?.getAttribute("title")).toContain("08:00:00.123");
    await fireEvent.click(screen.getByRole("button", { name: "按日期筛选" }));
    await fireEvent.input(screen.getByLabelText("日期"), { target: { value: "2026-09-29" } });
    expect((screen.getByLabelText("日期") as HTMLInputElement).value).toBe("2026-09-29");
    expect(screen.getByLabelText("日期").closest(".message-filter-fields")).toBeTruthy();
    expect(screen.getByRole("table").querySelectorAll("tbody tr")).toHaveLength(1);
    await fireEvent.click(screen.getByRole("button", { name: "按商品筛选" }));
    await fireEvent.change(screen.getByLabelText("商品"), { target: { value: "130" } });
    expect((screen.getByLabelText("商品") as HTMLSelectElement).value).toBe("130");
    expect(screen.getByLabelText("商品").closest(".message-filter-fields")).toBeTruthy();
    expect(screen.getByText("没有符合筛选条件的消息。")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "按日期筛选" }));
    expect(screen.getByRole("table").querySelectorAll("tbody tr")).toHaveLength(1);
    expect(screen.getByText("9/28")).toBeTruthy();
    expect(screen.getByText("08:00:00")).toBeTruthy();
    expect(screen.getByRole("table").querySelector("tbody td")?.getAttribute("title")).toContain("08:00:00.456");
    expect(screen.getByRole("table").querySelector("thead")?.classList.contains("sr-only")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "按商品筛选" }));
    expect(screen.getByRole("table").querySelectorAll("tbody tr")).toHaveLength(2);
  });

  it("最近消息只渲染视口行，长名称不撑宽且可滚动到末尾", async () => {
    const longName = "很长的商品名称".repeat(20);
    current.recentChecks = Array.from({ length: 100 }, (_, index) => ({
      productId: String(index), name: index === 0 ? longName : `商品 ${index}`,
      availability: "out_of_stock" as const, isShow: 0, stock: 0,
      at: new Date(Date.UTC(2026, 8, 30, 0, 0, index)).toISOString(),
    }));
    render(Page);
    const table = await screen.findByRole("table");
    await waitFor(() => expect(table.querySelectorAll("tbody tr").length).toBeGreaterThan(0));
    expect(table.querySelectorAll("tbody tr").length).toBeLessThan(100);
    const scroll = table.closest(".message-scroll") as HTMLDivElement;
    expect(scroll).toBeTruthy();
    scroll.scrollTop = 6200;
    await fireEvent.scroll(scroll);
    await waitFor(() => expect(screen.getByTitle(longName)).toBeTruthy());
    expect(pageSource).toMatch(/\.message-table th, \.message-table td \{[^}]*text-overflow: ellipsis/);
    expect(pageSource).toMatch(/\.message-table thead\.sr-only \{[^}]*clip-path: inset\(50%\)/);
    expect(pageSource).toMatch(/estimateSize: \(\) => 48/);
    expect(pageSource).toMatch(/\.message-table tr \{[^}]*height: 48px/);
  });

  it("消息翻页只保留当前页，筛选后从第一页重新查询", async () => {
    const firstItems = Array.from({ length: 100 }, (_, index) => ({ at: "2026-09-29T00:00:00Z", productId: "1", name: `商品 ${index}`, isShow: 0, stock: 0, detail: "商品下架" }));
    const nextCursor = { atMs: 1, source: 1, id: 1 };
    bridge.invoke.mockImplementation(async (name: string, args?: { query?: { date: string | null; cursor: unknown; limit: number } }) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") {
        expect(args!.query!.limit).toBe(100);
        if (args!.query!.date) return { items: [], nextCursor: null };
        return args!.query!.cursor ? { items: [{ ...firstItems[0], name: "第二页商品" }], nextCursor: null } : { items: firstItems, nextCursor };
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "下一页" });
    await fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("第二页商品")).toBeTruthy();
    expect(screen.getByText("第 2 页")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "按日期筛选" }));
    await fireEvent.input(screen.getByLabelText("日期"), { target: { value: "2026-09-28" } });
    await waitFor(() => expect(screen.queryByText("第二页商品")).toBeNull());
    expect(screen.queryByText("第 2 页")).toBeNull();
  });

  it("阅读已滚动的消息时保留位置，收到新消息后由用户刷新", async () => {
    current.recentChecks = [{ productId: "245", name: "GR IV HDF", availability: "out_of_stock", isShow: 0, stock: 0, at: "2026-09-28T10:00:00+08:00" }];
    const view = render(Page);
    await screen.findByRole("table");
    const scroller = view.container.querySelector<HTMLElement>(".message-scroll")!;
    scroller.scrollTop = 160;
    const before = bridge.invoke.mock.calls.filter(([name]) => name === "query_messages").length;
    current.recentChecks.unshift({ ...current.recentChecks[0], stock: 1, at: "2026-09-28T10:00:01+08:00" });
    bridge.eventHandler!({ payload: structuredClone(current) });
    await screen.findByRole("button", { name: "有新消息，刷新列表" });
    expect(scroller.scrollTop).toBe(160);
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "query_messages")).toHaveLength(before);
    await fireEvent.click(screen.getByRole("button", { name: "有新消息，刷新列表" }));
    await waitFor(() => expect(scroller.scrollTop).toBe(0));
  });

  it("第一步调整选择期间不写入后台，下一步一次提交最终商品集合", async () => {
    current.setupCompleted = false;
    current.catalog = structuredClone(onboardingCatalog);
    useOnboardingBridge();
    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("checkbox", { name: "官翻品 GR IIIx" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "GR SPACE VIP会员卡" }));
    expect(bridge.invoke.mock.calls.every(([name]) => name === "get_desktop_snapshot" || name === "query_messages")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "set_onboarding_products")).toHaveLength(1);
    const savedIds = bridge.invoke.mock.calls.find(([name]) => name === "set_onboarding_products")![1].productIds;
    expect(savedIds).toEqual(expect.arrayContaining(["19", "66", "130", "245", "108"]));
    expect(savedIds).toHaveLength(5);
    expect(savedIds).not.toContain("65");
    expect(bridge.invoke.mock.calls.some(([name]) => name === "monitoring_action")).toBe(false);
  });

  it("第一步保存待完成时锁住操作，保存失败保留选择和当前步骤", async () => {
    current.setupCompleted = false;
    current.catalog = structuredClone(onboardingCatalog);
    const pending = deferred<{ message: null }>();
    useOnboardingBridge();
    const originalInvoke = bridge.invoke.getMockImplementation()!;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "set_onboarding_products") return pending.promise;
      return originalInvoke(name, args);
    });
    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    const next = screen.getByRole("button", { name: "下一步" });
    await fireEvent.click(next);
    await fireEvent.click(next);
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "set_onboarding_products")).toHaveLength(1);
    expect(next.hasAttribute("disabled")).toBe(true);
    pending.reject(new Error("商品设置保存失败"));
    await waitFor(() => expect(screen.getByRole("button", { name: "下一步" }).hasAttribute("disabled")).toBe(false));
    expect(screen.getByRole("heading", { name: /选择监控商品/ })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: /设置通知/ })).toBeNull();
    expect((screen.getByRole("checkbox", { name: "官翻品 GR IV" }) as HTMLInputElement).checked).toBe(true);
    expect(current.products).toEqual([]);
    expect(current.setupCompleted).toBe(false);
  });

  it("引导通知页沿用渠道表单，保存并测试后留在第二步", async () => {
    current.setupCompleted = false;
    current.catalog = structuredClone(onboardingCatalog);
    current.providers = [{ id: "feishu", name: "飞书", documentationUrl: null, fields: [{ key: "appId", label: "应用 ID", type: "password", required: true, placeholder: null, help: null }] }];
    useOnboardingBridge();

    render(Page);
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    const dialog = screen.getByRole("dialog", { name: "添加飞书" });
    await fireEvent.input(within(dialog).getByLabelText("渠道名称"), { target: { value: "摄影值班群" } });
    await fireEvent.input(within(dialog).getByLabelText("应用 ID"), { target: { value: "fixture-app" } });
    await fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await screen.findByText("测试成功：平台已接受测试消息");
    expect(bridge.invoke).toHaveBeenCalledWith("save_notification_channel", { channel: expect.objectContaining({ name: "摄影值班群", providerId: "feishu", values: { appId: "fixture-app" } }) });
    expect(bridge.invoke).toHaveBeenCalledWith("test_notification_channel", { channelId: "saved-channel" });
    expect(screen.getByRole("heading", { name: /设置通知/ })).toBeTruthy();
    expect(current.setupCompleted).toBe(false);
  });

  it.each(["products", "catalog"])("手动输入已有编号时提示已在列表中，不重复请求（%s）", async (list) => {
    current[list as "products" | "catalog"] = [{ ...product, productId: "65", enabled: list === "products" }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    await fireEvent.click(screen.getByRole("button", { name: "查找商城商品" }));
    await fireEvent.input(screen.getByLabelText("Product ID"), { target: { value: "00065" } });
    await fireEvent.click(screen.getByRole("button", { name: "添加商品" }));

    expect((await screen.findByRole("status")).textContent).toContain("商品已在列表中。");
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "add_product")).toHaveLength(0);
  });

  it("显示最近投递状态和北京时间，不把平台接受说成用户已读", async () => {
    current.channels = [{
      ...channel,
      lastDelivery: {
        outcome: "accepted", event: "stock_available", at: "2026-09-29T00:00:00Z",
        message: "服务已接受通知",
      },
    }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));

    expect(document.querySelector('[title*="最近投递：已发送 · 商品上架或补货 · 9/29 08:00"]')).toBeTruthy();
    expect(screen.queryByText(/不代表用户已读/)).toBeNull();
    expect(screen.queryByText("服务已接受通知")).toBeNull();
  });

  it("通知卡片展示所选两群及各自结果，明确定位失败群", async () => {
    const recipients = [{ id: "chat-photo", kind: "chat", label: "摄影群" }, { id: "chat-stock", kind: "chat", label: "库存群" }];
    current.channels = [{ ...channel, selectedTargets: recipients, recipientDeliveries: [
      { target: recipients[0], lastDelivery: { outcome: "accepted", event: "test", at: "2026-10-07T00:00:00Z", message: "平台已接受" } },
      { target: recipients[1], lastDelivery: { outcome: "failed", event: "test", at: "2026-10-07T00:00:00Z", message: "机器人已离开群聊" } },
    ] }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    const results = document.querySelector('[aria-label="工作群接收对象"]')!;
    expect(within(results as HTMLElement).getByText("摄影群")).toBeTruthy();
    expect(within(results as HTMLElement).getByText("库存群")).toBeTruthy();
    expect(results.querySelector('[title*="机器人已离开群聊"]')).toBeTruthy();
    expect(results.children).toHaveLength(2);
  });

  it("已启用渠道未更改直接保存，不写配置或发送测试", async () => {
    current.providers = [{ id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: [] }];
    current.channels = [{ ...channel, connectionStatus: "ready", targets: [{ id: "chat-a", kind: "chat", label: "摄影群" }], selectedTargets: [{ id: "chat-a", kind: "chat", label: "摄影群" }] }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "编辑工作群" }));
    await fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "编辑飞书" })).toBeNull());
    expect(bridge.invoke).not.toHaveBeenCalledWith("save_notification_channel", expect.anything());
    expect(bridge.invoke).not.toHaveBeenCalledWith("test_notification_channel", expect.anything());
  });

  it("扫描结果可关闭，后台快照更新不会重新显示同一次结果", async () => {
    current.scan = { status: "completed", startId: "1", endId: "100", currentId: null, found: 75, error: null } as DesktopSnapshot["scan"];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect(screen.getByText("扫描完成 · —（1–100） · 找到 75")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "关闭扫描结果" }));
    bridge.eventHandler?.({ payload: structuredClone(current) });
    await waitFor(() => expect(screen.queryByText("扫描完成 · —（1–100） · 找到 75")).toBeNull());
  });

  it("保存计划时提交一份完整设置，并保留已保存的系统代理选择", async () => {
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_monitoring_config") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    await fireEvent.input(screen.getByLabelText("最小检查间隔（秒）"), { target: { value: "1.1" } });
    expect(screen.getByRole("button", { name: "重置更改" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("save_monitoring_config", {
      config: expect.objectContaining({ useSystemProxy: true, useProxyPool: false }),
    }));
  });

  it("未保存的速率修改可重置，切换页面后仍有明确的待保存状态", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    const save = screen.getByRole("button", { name: "保存设置" });
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    const minimum = screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement;
    await fireEvent.input(minimum, { target: { value: "1.2" } });
    expect(save.hasAttribute("disabled")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1.2");
    expect(screen.getByRole("button", { name: "重置更改" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "重置更改" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    expect(screen.queryByRole("button", { name: "重置更改" })).toBeNull();
    expect(screen.getByText(/每轮列表检查间隔在 1～2 秒之间随机选取/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: "失败提醒通知" })).toBeTruthy();
    expect(screen.getByText("星期")).toBeTruthy();
    expect(screen.queryByText(/开始时间包含/)).toBeNull();
  });

  it("数值可清空重新输入，未完成输入不写入配置", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    const minimum = screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement;
    await fireEvent.input(minimum, { target: { value: "" } });
    expect(minimum.value).toBe("");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    await fireEvent.input(minimum, { target: { value: "1.2" } });
    expect(minimum.value).toBe("1.2");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(false);
    await fireEvent.input(minimum, { target: { value: "" } });
    await fireEvent.blur(minimum);
    expect(minimum.value).toBe("1.2");
    await fireEvent.click(screen.getByRole("button", { name: "重置更改" }));
    expect(minimum.value).toBe("1");
  });

  it("状态页仅展示正在选中的监控商品", async () => {
    current.products = [product, { ...product, productId: "130", name: "已停用商品", enabled: false, runtimeError: "连接失败" }];
    current.runtime!.lastError = "连接失败";
    render(Page);
    await screen.findByText("GR IV HDF");
    expect(screen.queryByText("已停用商品")).toBeNull();
    expect(screen.getByRole("button", { name: "监控异常" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect(screen.getByRole("checkbox", { name: "已停用商品" })).toBeTruthy();
  });

  it("速率与网络草稿分别保存和重置，另一页草稿不随保存提交", async () => {
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_monitoring_config") {
        current.config = structuredClone(args?.config);
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    await fireEvent.input(screen.getByLabelText("最小检查间隔（秒）"), { target: { value: "1.2" } });
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    await fireEvent.click(screen.getByRole("checkbox", { name: "使用代理池" }));
    await fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect(current.config?.useProxyPool).toBe(true));
    expect(current.config?.rate.intervalMinMs).toBe(1000);
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1.2");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "重置更改" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1");
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
  });

  it("速率和网络改回原值后各自恢复不可保存", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    const minimum = screen.getByLabelText("最小检查间隔（秒）");
    await fireEvent.input(minimum, { target: { value: "1.3" } });
    await fireEvent.input(minimum, { target: { value: "1" } });
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    const pool = screen.getByRole("checkbox", { name: "使用代理池" });
    await fireEvent.click(pool);
    await fireEvent.click(pool);
    await fireEvent.click(screen.getByRole("checkbox", { name: "使用系统代理" }));
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
  });

  it("保存速率不会提交网络草稿，重置网络也保留速率草稿", async () => {
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_monitoring_config") {
        current.config = structuredClone(args?.config);
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "使用代理池" }));
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    await fireEvent.input(screen.getByLabelText("最小检查间隔（秒）"), { target: { value: "1.4" } });
    await fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect(current.config?.rate.intervalMinMs).toBe(1400));
    expect(current.config?.useProxyPool).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect((screen.getByRole("checkbox", { name: "使用代理池" }) as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "重置更改" }));
    expect((screen.getByRole("checkbox", { name: "使用代理池" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1.4");
  });

  it("恢复默认后清除未保存的时间和速率草稿，完成引导使用全天计划", async () => {
    useOnboardingBridge();
    const originalInvoke = bridge.invoke.getMockImplementation()!;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "restore_defaults") {
        current = makeSnapshot({ setupCompleted: false, catalog: structuredClone(onboardingCatalog) });
        return { message: null };
      }
      return originalInvoke(name, args);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    await fireEvent.input(screen.getByLabelText("开始时间"), { target: { value: "10:30" } });
    await fireEvent.input(screen.getByLabelText("最小检查间隔（秒）"), { target: { value: "2.1" } });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "恢复默认设置…" }));
    await fireEvent.click(screen.getByRole("button", { name: "确认恢复" }));

    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    await screen.findByRole("heading", { name: /恭喜！/ });
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    await screen.findByRole("heading", { name: "正在监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect((screen.getByLabelText("开始时间") as HTMLInputElement).value).toBe("00:00");
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    expect(screen.queryByRole("button", { name: "重置更改" })).toBeNull();
  });

  it("系统测试通知只需发送一次，不要求用户重复确认可见性", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "发送测试" }));

    expect(bridge.invoke.mock.calls.filter(([name]) => name === "test_system_notification")).toHaveLength(1);
    expect(await screen.findByText("系统已接受测试通知。")).toBeTruthy();
  });

  it("通知页面以紧凑列表和弹窗管理渠道，名称和凭据在弹窗中编辑", async () => {
    current.channels = [channel];
    current.providers = [{
      id: "feishu", name: "飞书", documentationUrl: null,
      fields: [{ key: "appId", label: "应用 ID", type: "password", required: true, placeholder: "你的应用 ID", help: null }],
    }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.getByRole("checkbox", { name: "启用系统通知" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "添加飞书" }).querySelector("img")?.getAttribute("src")).toBe("/providers/feishu.png");
    expect(screen.getByRole("button", { name: "测试工作群" })).toBeTruthy();
    expect(screen.queryByText(/持续有货/)).toBeNull();

    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    expect(screen.getByRole("dialog", { name: "添加飞书" })).toBeTruthy();
    expect(screen.queryByRole("radio")).toBeNull();
    expect(screen.getByLabelText("应用 ID")).toBeTruthy();
    expect(screen.getByLabelText("渠道名称")).toBeTruthy();
    for (const label of ["商品上架或补货", "监控异常", "监控恢复"]) expect(screen.getByLabelText(label)).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("删除渠道先说明名称和凭据后果，取消保留，确认才删除", async () => {
    current.channels = [channel];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "remove_notification_channel") {
        current.channels = [];
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "删除工作群" }));
    const confirmation = screen.getByRole("group", { name: "确认删除工作群" });
    expect(confirmation.textContent).toContain("工作群");
    expect(confirmation.textContent).toContain("凭据也会永久删除");
    expect(document.activeElement).toBe(confirmation);
    expect(bridge.invoke).not.toHaveBeenCalledWith("remove_notification_channel", expect.anything());
    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.queryByRole("group", { name: "确认删除工作群" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "删除工作群" }));
    await fireEvent.click(screen.getByRole("button", { name: "取消删除" }));
    expect(screen.queryByRole("group", { name: "确认删除工作群" })).toBeNull();
    expect(screen.getByText("工作群")).toBeTruthy();
    expect(bridge.invoke).not.toHaveBeenCalledWith("remove_notification_channel", expect.anything());

    await fireEvent.click(screen.getByRole("button", { name: "删除工作群" }));
    await fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("remove_notification_channel", { channelId: channel.id }));
    await waitFor(() => expect(screen.queryByText("工作群")).toBeNull());
  });

  it("渠道测试将 failed、not_configured、invalid 作为失败并展示返回说明", async () => {
    current.channels = [channel];
    const outcomes = ["failed", "not_configured", "invalid"] as const;
    let index = 0;
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "test_notification_channel") return { outcome: outcomes[index++], message: `测试说明 ${index}` };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    for (let i = 1; i <= outcomes.length; i += 1) {
      await fireEvent.click(screen.getByRole("button", { name: "测试工作群" }));
      expect((await screen.findByRole("alert")).textContent).toContain(`测试说明 ${i}`);
      expect(screen.queryByRole("status")).toBeNull();
      if (i < outcomes.length) await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    }
  });

  it("渠道测试成功明确提示平台已接受，渠道表单按 Core 字段显示凭据说明和文档入口", async () => {
    current.providers = [
      { id: "feishu", name: "飞书", documentationUrl: "https://example.test/feishu", fields: [{ key: "appId", label: "应用 ID", type: "password", required: true, placeholder: "你的应用 ID", help: null }, { key: "appSecret", label: "应用密钥", type: "password", required: true, placeholder: "填写应用密钥", help: "填写平台应用凭据" }] },
      { id: "wecom", name: "企业微信", documentationUrl: "https://example.test/wecom", fields: [{ key: "botId", label: "机器人 ID", type: "password", required: true, placeholder: "你的机器人 ID", help: null }, { key: "secret", label: "机器人密钥", type: "password", required: true, placeholder: null, help: null }] },
      { id: "dingtalk", name: "钉钉", documentationUrl: "https://example.test/dingtalk", fields: [{ key: "appId", label: "应用 ID", type: "password", required: true, placeholder: "你的应用 ID", help: null }, { key: "appSecret", label: "应用密钥", type: "password", required: true, placeholder: "填写应用密钥", help: "填写平台应用凭据" }] },
    ];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "test_notification_channel") return { outcome: "accepted", message: "平台已接受" };
      if (name === "open_external_url") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    current.channels = [channel];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "测试工作群" }));
    expect((await screen.findByRole("status")).textContent).toContain("测试成功：平台已接受");

    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    expect(screen.getByLabelText("应用 ID")).toBeTruthy();
    expect(screen.getByLabelText("应用密钥")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "查看飞书机器人配置说明 ↗" }));
    expect(bridge.invoke).toHaveBeenCalledWith("open_external_url", { url: "https://example.test/feishu" });
    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));

    await fireEvent.click(screen.getByRole("button", { name: "添加企业微信" }));
    expect(screen.getByLabelText("机器人 ID")).toBeTruthy();
    expect(screen.getByLabelText("机器人密钥")).toBeTruthy();
    expect(screen.queryByLabelText("应用密钥")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));

    await fireEvent.click(screen.getByRole("button", { name: "添加钉钉" }));
    expect(screen.getByLabelText("应用 ID")).toBeTruthy();
    expect(screen.getByLabelText("应用密钥")).toBeTruthy();
  });

  it("通知渠道提交名称、三种提醒、SDK 应用凭据，并可保存后测试", async () => {
    current.providers = [{
      id: "feishu", name: "飞书", documentationUrl: "https://example.test/feishu",
      fields: [
        { key: "appId", label: "应用 ID", type: "password", required: true, placeholder: "你的应用 ID", help: null },
        { key: "appSecret", label: "应用密钥", type: "password", required: true, placeholder: "填写应用密钥", help: "填写平台应用凭据" },
      ],
    }];
    let saved: Record<string, any> | null = null;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_notification_channel") {
        saved = args?.channel;
        return { ...channel, id: "saved-channel", name: "飞书机器人", configuredFieldKeys: ["appId", "appSecret"] };
      }
      if (name === "test_notification_channel") return { outcome: "accepted", message: "平台已接受测试消息" };
      if (name === "set_notification_channel_enabled") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    await fireEvent.input(screen.getByLabelText("渠道名称"), { target: { value: "摄影值班群" } });
    await fireEvent.input(screen.getByLabelText("应用 ID"), { target: { value: "fixture-app" } });
    await fireEvent.input(screen.getByLabelText("应用密钥"), { target: { value: "fixture-secret" } });
    await fireEvent.click(screen.getByLabelText("监控异常"));
    await fireEvent.click(screen.getByLabelText("监控恢复"));
    await fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await screen.findByText("测试成功：平台已接受测试消息");
    expect(saved).toMatchObject({
      name: "摄影值班群", providerId: "feishu",
      values: { appId: "fixture-app", appSecret: "fixture-secret" },
      subscriptions: ["stock_available", "monitoring_failed", "recovered"],
    });
    expect(bridge.invoke).toHaveBeenCalledWith("test_notification_channel", { channelId: "saved-channel" });
  });

  it("渠道保存错误在弹窗内提示，关闭后不留全局错误", async () => {
    current.providers = [{
      id: "feishu", name: "飞书", documentationUrl: null,
      fields: [{ key: "appId", label: "应用 ID", type: "password", required: true, placeholder: null, help: null }],
    }];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_notification_channel") throw "通知渠道凭据无效，请检查应用 ID";
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    const dialog = screen.getByRole("dialog", { name: "添加飞书" });
    await fireEvent.click(screen.getByRole("button", { name: /^保存$/ }));
    expect(dialog.querySelector('[role="alert"]')?.textContent).toBe("请填写应用 ID。");
    expect(screen.getAllByRole("alert")).toHaveLength(1);

    await fireEvent.input(screen.getByLabelText("应用 ID"), { target: { value: "invalid" } });
    await fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(dialog.querySelector('[role="alert"]')?.textContent).toBe("通知渠道凭据无效，请检查应用 ID"));
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(bridge.invoke).not.toHaveBeenCalledWith("test_notification_channel", expect.anything());

    await fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("alert")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    expect(screen.getByRole("dialog").querySelector('[role="alert"]')).toBeNull();
  });

  it("扫码后检测并选择两群，保存完整目标数组并发送测试消息", async () => {
    current.providers = [{ id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: [{ key: "appSecret", label: "应用密钥", type: "password", required: true, placeholder: null, help: null }] }];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "begin_channel_binding") return { id: "binding-1", provider: "feishu", status: "complete", connectionStatus: "ready", targets: [{ id: "chat-1", kind: "chat", label: "摄影群" }, { id: "user-1", kind: "user", label: "我的账号" }] };
      if (name === "detect_binding_groups") return [{ id: "chat-1", kind: "chat", label: "摄影群" }, { id: "chat-2", kind: "chat", label: "库存群" }];
      if (name === "save_notification_channel") {
        const saved = { ...channel, id: "saved-channel", name: args!.channel.name, enabled: false, connectionStatus: "ready" };
        current.channels.push(saved);
        return saved;
      }
      if (name === "test_notification_channel") return { outcome: "accepted", message: "平台已接受" };
      if (name === "set_notification_channel_enabled") return { message: null };
      if (name === "cancel_channel_binding") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.getByText("尚未添加通知渠道。添加后可向所选会话发送库存提醒。").classList.contains("channel-empty")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    await screen.findByRole("checkbox", { name: "摄影群 群聊" });
    expect(screen.getByLabelText("渠道名称").getAttribute("placeholder")).toBe("飞书 1");
    expect(screen.queryByLabelText("应用密钥")).toBeNull();
    expect((screen.getByRole("checkbox", { name: "我的账号 个人" }) as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await screen.findByRole("checkbox", { name: "库存群 群聊" });
    await fireEvent.click(screen.getByRole("checkbox", { name: "我的账号 个人" }));
    await fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(screen.getByRole("alert").textContent).toContain("请勾选至少一个通知接收对象");
    expect(bridge.invoke).not.toHaveBeenCalledWith("save_notification_channel", expect.anything());
    await fireEvent.click(screen.getByRole("checkbox", { name: "摄影群 群聊" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "库存群 群聊" }));
    await fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("test_notification_channel", { channelId: "saved-channel" }));
    expect(bridge.invoke).toHaveBeenCalledWith("detect_binding_groups", { bindingId: "binding-1" });
    expect(bridge.invoke).toHaveBeenCalledWith("save_notification_channel", { channel: expect.objectContaining({ name: "飞书 1", bindingId: "binding-1", values: {}, targets: [{ id: "chat-1", kind: "chat", label: "摄影群" }, { id: "chat-2", kind: "chat", label: "库存群" }] }) });
    expect(bridge.invoke).toHaveBeenCalledWith("cancel_channel_binding", { bindingId: "binding-1" });
  });

  it.each([true, false])("编辑回填已选群，取消保留配置，保存多群并保留启用意图（%s）", async (enabled) => {
    const discovered = [{ id: "chat-1", kind: "chat", label: "摄影群" }, { id: "chat-2", kind: "chat", label: "库存群" }];
    current.providers = [{ id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: [] }];
    current.channels = [{ ...channel, enabled, connectionStatus: "ready", targets: discovered, selectedTargets: [discovered[0]] }];
    let submitted: Record<string, any> | null = null;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "save_notification_channel") { submitted = args!.channel; return current.channels[0]; }
      if (["begin_channel_editing", "end_channel_editing", "set_notification_channel_enabled"].includes(name)) return { message: null };
      if (name === "test_notification_channel") return { outcome: "accepted", message: "平台已接受" };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "编辑工作群" }));
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "库存群 群聊" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(screen.getByRole("checkbox", { name: "库存群 群聊" }));
    await fireEvent.click(within(screen.getByRole("dialog", { name: "编辑飞书" })).getByRole("button", { name: "取消" }));
    expect(current.channels[0].selectedTargets).toEqual([discovered[0]]);
    await fireEvent.click(screen.getByRole("button", { name: "编辑工作群" }));
    expect((screen.getByRole("checkbox", { name: "库存群 群聊" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(screen.getByRole("checkbox", { name: "库存群 群聊" }));
    await fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(submitted).not.toBeNull());
    expect(submitted!.targets).toEqual(discovered);
    expect(submitted!.targets).not.toBe(discovered);
    expect(submitted!.targets[0]).not.toBe(discovered[0]);
    expect(submitted!.values).toEqual({});
    expect(submitted!.name).toBe("工作群");
    expect(JSON.stringify(submitted)).not.toMatch(/appSecret|botToken|qrUrl/);
    expect(current.channels[0].selectedTargets).toEqual([discovered[0]]);
    expect(bridge.invoke).not.toHaveBeenCalledWith("begin_channel_binding", expect.anything());
    expect(bridge.invoke).not.toHaveBeenCalledWith("test_notification_channel", { channelId: channel.id });
    expect(bridge.invoke).not.toHaveBeenCalledWith("set_notification_channel_enabled", expect.anything());
  });

  it.each(["accepted", "failed", "unknown"])("扫码弹窗固定外部平台，保存私信按验证结果启用（%s）", async (outcome) => {
    current.providers = [{ id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: [] }];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "begin_channel_binding") return { id: "binding-1", provider: "feishu", status: "complete", connectionStatus: "ready", targets: [{ id: "my-user", kind: "user", label: "我的账号" }] };
      if (name === "save_notification_channel") return { ...channel, id: "saved-channel", enabled: false };
      if (name === "test_notification_channel") return { outcome, message: "平台返回说明" };
      if (name === "set_notification_channel_enabled" || name === "cancel_channel_binding") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    const dialog = await screen.findByRole("dialog", { name: "添加飞书" });
    await within(dialog).findByRole("img", { name: "已连接飞书" });
    expect(within(dialog).queryByRole("radio")).toBeNull();
    expect(within(dialog).getByRole("textbox", { name: "渠道名称" }).hasAttribute("readonly")).toBe(true);
    expect(within(dialog).queryByRole("button", { name: "保存并测试" })).toBeNull();
    await fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("test_notification_channel", { channelId: "saved-channel" }));
    if (outcome === "accepted") await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("set_notification_channel_enabled", { channelId: "saved-channel", enabled: true }));
    else {
      await screen.findByRole("alert");
      expect(bridge.invoke).not.toHaveBeenCalledWith("set_notification_channel_enabled", expect.anything());
    }
    expect(bridge.invoke).toHaveBeenCalledWith("save_notification_channel", { channel: expect.objectContaining({ providerId: "feishu", bindingId: "binding-1", targets: [{ id: "my-user", kind: "user", label: "我的账号" }] }) });
  });

  it("重跑引导仅保存启动偏好，当前监控保持原样", async () => {
    current.products = [{ ...product, name: "官翻品 GR IIIx" }];
    current.runtime!.state = "monitoring";
    useOnboardingBridge();
    render(Page);
    await screen.findByRole("heading", { name: "正在监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "帮助引导" }));
    await screen.findByRole("heading", { name: /选择监控商品/ });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await screen.findByRole("heading", { name: /设置通知/ });
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "启动后自动开启监控" }));
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    await screen.findByRole("heading", { name: "正在监控" });
    expect(current.config!.autoStartMonitoring).toBe(false);
    expect(bridge.invoke).toHaveBeenCalledWith("set_auto_start_monitoring", { enabled: false });
    expect(bridge.invoke.mock.calls.some(([name]) => name === "complete_setup" || name === "monitoring_action")).toBe(false);
  });

  it("自动监控设置与登录启动独立，修改后不改变当前监控", async () => {
    current.runtime!.state = "monitoring";
    const originalInvoke = bridge.invoke.getMockImplementation()!;
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "set_auto_start_monitoring") { current.config!.autoStartMonitoring = args!.enabled; return { message: null }; }
      return originalInvoke(name, args);
    });
    render(Page);
    await screen.findByRole("heading", { name: "正在监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    const auto = screen.getByRole("checkbox", { name: "启动后自动开启监控" }) as HTMLInputElement;
    expect(auto.checked).toBe(true);
    await fireEvent.click(auto);
    await waitFor(() => expect(current.config!.autoStartMonitoring).toBe(false));
    expect(current.platform!.loginStartEnabled).toBe(false);
    expect(current.runtime!.state).toBe("monitoring");
    expect(bridge.invoke).toHaveBeenCalledWith("set_auto_start_monitoring", { enabled: false });
    expect(bridge.invoke.mock.calls.some(([name]) => ["set_login_start", "monitoring_action", "save_monitoring_config"].includes(name))).toBe(false);
  });

  it("扫码完成才展示提醒内容，手动配置名称在顶部且无需展开", async () => {
    const pairing = deferred<any>();
    current.providers = [{ id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: [{ key: "appId", label: "应用 ID", type: "text", required: true, placeholder: null, help: null }] }];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage({ date: null, productId: null, limit: 100 });
      if (name === "begin_channel_binding") return pairing.promise;
      if (name === "cancel_channel_binding") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    const dialog = screen.getByRole("dialog", { name: "添加飞书" });
    expect(within(dialog).queryByRole("group", { name: "提醒内容" })).toBeNull();
    expect(within(dialog).queryByText("保存后发送测试通知，成功后自动启用。")).toBeNull();
    expect((within(dialog).getByRole("button", { name: "保存" }) as HTMLButtonElement).disabled).toBe(true);
    pairing.resolve({ id: "binding-1", provider: "feishu", status: "complete", connectionStatus: "ready", targets: [{ id: "user-1", kind: "user", label: "我的账号" }] });
    await within(dialog).findByRole("group", { name: "提醒内容" });
    expect(within(dialog).queryByText("通知范围")).toBeNull();
    expect(within(dialog).getByRole("checkbox", { name: "商品上架或补货" }).closest("details")).toBeNull();
    await fireEvent.click(within(dialog).getByRole("button", { name: "手动配置" }));
    const title = within(dialog).getByLabelText("渠道名称");
    const appId = within(dialog).getByLabelText("应用 ID");
    expect(title.closest("details")).toBeNull();
    expect(title.compareDocumentPosition(appId) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("切换手动配置和关闭弹窗取消二维码绑定", async () => {
    current.providers = [{ id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: [{ key: "appId", label: "应用 ID", type: "text", required: true, placeholder: null, help: null }] }];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "query_messages") return messagePage(args!.query!);
      if (name === "begin_channel_binding") return { id: "binding-1", provider: "feishu", status: "waiting", qrUrl: "https://official.example/registration", targets: [] };
      if (name === "cancel_channel_binding") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    await screen.findByAltText("使用飞书扫描二维码");
    await fireEvent.click(screen.getByRole("button", { name: "手动配置" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("cancel_channel_binding", { bindingId: "binding-1" }));
    expect(screen.getByLabelText("应用 ID")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "扫码连接" }));
    await screen.findByAltText("使用飞书扫描二维码");
    await fireEvent(screen.getByRole("dialog", { name: "添加飞书" }), new Event("cancel", { cancelable: true }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "添加飞书" })).toBeNull());
    expect(bridge.invoke.mock.calls.filter(([name]) => name === "cancel_channel_binding")).toHaveLength(2);
  });

  it("商品目录与已监控商品合并去重，并在同一行管理", async () => {
    current.products = [product];
    current.catalog = [product, { ...product, productId: "65", name: "GR IIIx", enabled: false }];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "set_product_enabled" || name === "remove_product") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect(screen.getAllByText("ID 245")).toHaveLength(1);
    expect(screen.getByRole("checkbox", { name: "GR IV HDF" })).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "GR IIIx" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("checkbox", { name: "GR IIIx" }));
    expect(bridge.invoke).toHaveBeenCalledWith("set_product_enabled", { productId: "65", enabled: true });
    expect(screen.queryByText("目录")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "查找商城商品" }));
    expect(screen.getByRole("button", { name: "开始扫描" })).toBeTruthy();
    expect(screen.getByLabelText("从")).toBeTruthy();
  });

  it("商品目录默认稳定按产品编号排序，不按监控状态重排", async () => {
    const first = { ...product, productId: "65", name: "官翻品 GR IIIx" };
    const second = { ...product, productId: "130", name: "官翻品 GR IV" };
    current.products = [first, second];
    current.catalog = [{ ...product, productId: "38", name: "日记版", enabled: false }, first, { ...product, productId: "108", name: "会员卡", enabled: false }, second];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect([...document.querySelectorAll(".product-choice-grid .product-name")].map((node) => node.textContent)).toEqual(["日记版", "官翻品 GR IIIx", "会员卡", "官翻品 GR IV"]);
  });

  it("商品可按名称或编号搜索，并筛选已选与未选", async () => {
    current.products = [product];
    current.catalog = [product, { ...product, productId: "65", name: "GR IIIx", enabled: false }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    await fireEvent.change(screen.getByRole("combobox", { name: "商品筛选" }), { target: { value: "selected" } });
    expect(screen.getByRole("checkbox", { name: "GR IV HDF" })).toBeTruthy();
    expect(screen.queryByRole("checkbox", { name: "GR IIIx" })).toBeNull();
    await fireEvent.change(screen.getByRole("combobox", { name: "商品筛选" }), { target: { value: "unselected" } });
    expect(screen.getByRole("checkbox", { name: "GR IIIx" })).toBeTruthy();
    expect(screen.getByRole("textbox", { name: "搜索名称或 Product ID" }).closest(".product-list-heading")).toBeTruthy();
    await fireEvent.input(screen.getByRole("textbox", { name: "搜索名称或 Product ID" }), { target: { value: "245" } });
    expect(screen.getByText("没有符合条件的商品。")).toBeTruthy();
    await fireEvent.change(screen.getByRole("combobox", { name: "商品筛选" }), { target: { value: "all" } });
    expect(screen.getByRole("checkbox", { name: "GR IV HDF" })).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "清除商品搜索" }));
    expect((screen.getByRole("textbox", { name: "搜索名称或 Product ID" }) as HTMLInputElement).value).toBe("");
    expect(pageSource).toMatch(/\.product-management \.product-list-heading \.product-search input \{[^}]*padding-left: 34px; padding-right: 28px/);
    expect(pageSource).toMatch(/\.product-management \.product-toolbar \{[^}]*background: transparent/);
  });

  it("取消监控保留商品，再次添加监控复用同一行", async () => {
    current.products = [product];
    current.catalog = [product];
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "set_product_enabled") {
        const enabled = current.products.length === 0;
        current.products = enabled ? [product] : [];
        current.catalog = enabled ? [] : [{ ...product, enabled: false }];
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect((screen.getByRole("checkbox", { name: "GR IV HDF" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.queryByRole("button", { name: "移除GR IV HDF" })).toBeNull();
    await fireEvent.click(screen.getByRole("checkbox", { name: "GR IV HDF" }));
    await waitFor(() => expect((screen.getByRole("checkbox", { name: "GR IV HDF" }) as HTMLInputElement).checked).toBe(false));
    expect(bridge.invoke).toHaveBeenCalledWith("set_product_enabled", { productId: "245", enabled: false });
    expect(bridge.invoke).not.toHaveBeenCalledWith("remove_product", expect.anything());

    await fireEvent.click(screen.getByRole("button", { name: "状态" }));
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "GR IV HDF" }));
    await waitFor(() => expect((screen.getByRole("checkbox", { name: "GR IV HDF" }) as HTMLInputElement).checked).toBe(true));
    expect(screen.getByText("GR IV HDF")).toBeTruthy();
    expect(bridge.invoke).not.toHaveBeenCalledWith("remove_product", expect.anything());

    expect(bridge.invoke).toHaveBeenCalledWith("set_product_enabled", { productId: "245", enabled: true });
    expect(screen.getAllByText("GR IV HDF")).toHaveLength(1);
  });

  it("系统通知保存失败时恢复原状态并显示错误", async () => {
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "set_system_notifications_enabled") throw new Error("Command set_system_notifications_enabled not found");
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    const toggle = screen.getByRole("checkbox", { name: "启用系统通知" }) as HTMLInputElement;
    expect(toggle.checked).toBe(false);

    await fireEvent.click(toggle);

    expect((await screen.findByRole("alert")).textContent).toContain("Command set_system_notifications_enabled not found");
    await waitFor(() => expect(toggle.checked).toBe(false));
  });

  it("系统权限缺失时不提示首次发送会询问", async () => {
    current.platform = null;
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    expect(screen.getByText("权限：尚未确认")).toBeTruthy();
  });

  it("扫描商品编号并在结果中提供选择", async () => {
    const scan = { id: "scan-1", startId: "100", endId: "110", currentId: "106", checked: 2, found: 1, status: "running" as const, error: null, results: [product] };
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "start_product_scan") {
        current.scan = scan;
        current.catalog = [{ ...product, enabled: false }];
        return scan;
      }
      if (name === "set_product_enabled") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    await fireEvent.click(screen.getByRole("button", { name: "查找商城商品" }));
    await fireEvent.input(screen.getByLabelText("从"), { target: { value: "100" } });
    await fireEvent.input(screen.getByLabelText("到"), { target: { value: "110" } });
    await fireEvent.click(screen.getByRole("button", { name: "开始扫描" }));

    expect(await screen.findByText("GR IV HDF")).toBeTruthy();
    expect(screen.getByText("扫描中 · 106（100–110） · 找到 1")).toBeTruthy();
    expect(screen.queryByText(/已检查 2/)).toBeNull();
    expect(bridge.invoke).toHaveBeenCalledWith("start_product_scan", { startId: "100", endId: "110" });
  });

  it("启用代理池后保存其出口选择", async () => {
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "save_monitoring_config") return { message: null };
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "使用代理池" }));
    await fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("save_monitoring_config", {
      config: expect.objectContaining({ useProxyPool: true }),
    }));
  });

  it("代理新增隐藏在列表右侧弹窗，可切换单个与批量导入", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect(screen.queryByLabelText("主机")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "添加代理" }));
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.getByLabelText("主机")).toBeTruthy();
    await fireEvent.keyDown(screen.getByRole("tab", { name: "添加单个代理" }), { key: "ArrowRight" });
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("tab", { name: "批量导入" })));
    expect(screen.getByRole("tab", { name: "批量导入" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.queryByRole("textbox", { name: "主机" })).toBeNull();
    expect(screen.getByPlaceholderText("每行一条代理").closest("[hidden]")).toBeNull();
    expect(screen.getByRole("tabpanel").getAttribute("aria-labelledby")).toBe("proxy-batch-tab");
    await fireEvent.keyDown(screen.getByRole("tab", { name: "批量导入" }), { key: "Home" });
    expect(screen.getByLabelText("主机")).toBeTruthy();
  });

  it("小窗口弹窗将操作栏留在滚动内容外，监控操作有明确状态", async () => {
    vi.stubGlobal("innerWidth", 720);
    vi.stubGlobal("innerHeight", 560);
    current.products = [product];
    current.catalog = [product];
    current.providers = [{ id: "feishu", name: "飞书", fields: [], documentationUrl: null }];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });

    await fireEvent.click(screen.getByRole("button", { name: "通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加飞书" }));
    const channelDialog = screen.getByRole("dialog", { name: "添加飞书" });
    expect(channelDialog.querySelector(".modal-body")).toBeTruthy();
    expect(channelDialog.querySelector(".modal-actions")?.parentElement).toBe(channelDialog);
    expect(pageSource).toMatch(/\.modal-body\s*\{[^}]*overflow-y:\s*auto/);
    expect(pageSource).toMatch(/\.channel-modal \.modal-body\s*\{[^}]*flex:\s*0 1 auto/);

    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加代理" }));
    const proxyDialog = screen.getByRole("dialog", { name: "添加代理" });
    expect(proxyDialog.querySelector(".modal-actions")?.parentElement).toBe(proxyDialog);
    await fireEvent.click(screen.getByRole("tab", { name: "批量导入" }));
    expect(proxyDialog.querySelector(".modal-actions")?.textContent).toContain("导入");

    await fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    await fireEvent.click(screen.getByRole("button", { name: "监控产品" }));
    expect((screen.getByRole("checkbox", { name: "GR IV HDF" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByRole("checkbox", { name: "GR IV HDF" }).closest(".product-option")?.classList.contains("selected")).toBe(true);
  });

  it("代理停用原因明确呈现", async () => {
    current.proxies = [
      { id: "auto", protocol: "http", displayAddress: "192.0.2.1:8080", enabled: false, status: "auto_disabled" },
      { id: "manual", protocol: "http", displayAddress: "192.0.2.2:8080", enabled: false, status: "manually_disabled" },
    ];
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect(screen.getByText("因连续失败自动停用")).toBeTruthy();
    expect(screen.getByText("已手动停用")).toBeTruthy();
    expect(screen.queryByText("尚未测试")).toBeNull();
  });

  it("代理删除须先确认，取消保留，确认后才删除", async () => {
    current.proxies = [{ id: "proxy-1", protocol: "http", displayAddress: "192.0.2.1:8080", enabled: true, status: "available" }];
    bridge.invoke.mockImplementation(async (name: string, args?: Record<string, any>) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "remove_proxy") {
        current.proxies = [];
        return { message: null };
      }
      throw new Error(`Unexpected command: ${name}`);
    });

    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    await fireEvent.click(screen.getByRole("button", { name: "删除代理192.0.2.1:8080" }));
    const confirmation = screen.getByRole("group", { name: "确认删除代理192.0.2.1:8080" });
    expect(document.activeElement).toBe(confirmation);
    expect(bridge.invoke).not.toHaveBeenCalledWith("remove_proxy", expect.anything());

    await fireEvent.click(screen.getByRole("button", { name: "取消删除" }));
    expect(screen.queryByRole("group", { name: "确认删除代理192.0.2.1:8080" })).toBeNull();
    expect(screen.getByText("192.0.2.1:8080")).toBeTruthy();
    expect(bridge.invoke).not.toHaveBeenCalledWith("remove_proxy", expect.anything());

    await fireEvent.click(screen.getByRole("button", { name: "删除代理192.0.2.1:8080" }));
    await fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(bridge.invoke).toHaveBeenCalledWith("remove_proxy", { proxyId: "proxy-1" }));
  });

  it("其他设置直接展示导入导出、诊断和恢复默认", async () => {
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    for (const label of ["配置文件", "诊断报告", "恢复默认设置"]) {
      expect(screen.getByText(label).closest("details")).toBeNull();
    }
    expect(screen.getByRole("button", { name: "导出配置…" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "生成预览" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "恢复默认设置…" })).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "相关链接" })).toBeNull();
    expect(screen.queryByText("导入/导出、诊断与恢复默认")).toBeNull();
    expect(screen.queryByRole("textbox", { name: "导入配置" })).toBeNull();
    expect(screen.getByText(/导入会合并商品和通知渠道/).textContent).toContain("导入后监控停止");
    expect(screen.getByText(/导入会合并商品和通知渠道/).textContent).toContain("导出不含通知凭据");
    expect(screen.getByText(/导入会合并商品和通知渠道/).textContent).toContain("重新测试后启用");
  });

  it("配置导入导出交给桌面文件选择器，取消不报成功", async () => {
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "export_configuration_file") return "/tmp/ricoh-monitor-config.json";
      if (name === "import_configuration_file") return null;
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "导出配置…" }));
    expect((await screen.findByRole("status")).textContent).toContain("配置已导出。");
    expect(screen.getByRole("status").classList.contains("toast")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "导入配置…" }));
    expect(screen.queryByRole("status")).toBeNull();
    expect(bridge.invoke).toHaveBeenCalledWith("import_configuration_file", undefined);
  });

  it("导入配置后丢弃旧的未保存草稿", async () => {
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "import_configuration_file") {
        current.config!.rate.intervalMinMs = 1800;
        return "/tmp/ricoh-monitor-config.json";
      }
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    await fireEvent.input(screen.getByLabelText("最小检查间隔（秒）"), { target: { value: "1.4" } });
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(false);
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "使用代理池" }));
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "导入配置…" }));
    await screen.findByText("配置已导入。");
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1.8");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "代理池" }));
    expect((screen.getByRole("checkbox", { name: "使用代理池" }) as HTMLInputElement).checked).toBe(false);
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(true);
  });

  it.each(["取消", "失败"])("导入%s保留未保存的速率草稿", async (outcome) => {
    const pending = deferred<string | null>();
    bridge.invoke.mockImplementation(async (name: string) => {
      if (name === "get_desktop_snapshot") return structuredClone(current);
      if (name === "import_configuration_file") return pending.promise;
      throw new Error(`Unexpected command: ${name}`);
    });
    render(Page);
    await screen.findByRole("button", { name: "开始监控" });
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    await fireEvent.input(screen.getByLabelText("最小检查间隔（秒）"), { target: { value: "1.4" } });
    await fireEvent.click(screen.getByRole("button", { name: "其他设置" }));
    await fireEvent.click(screen.getByRole("button", { name: "导入配置…" }));
    if (outcome === "取消") pending.resolve(null);
    else pending.reject(new Error("配置文件无效"));
    if (outcome === "失败") expect((await screen.findByRole("alert")).textContent).toContain("配置文件无效");
    else await waitFor(() => expect(screen.getByRole("button", { name: "导入配置…" }).hasAttribute("disabled")).toBe(false));
    await fireEvent.click(screen.getByRole("button", { name: "速率" }));
    expect((screen.getByLabelText("最小检查间隔（秒）") as HTMLInputElement).value).toBe("1.4");
    expect(screen.getByRole("button", { name: "保存设置" }).hasAttribute("disabled")).toBe(false);
  });
});
