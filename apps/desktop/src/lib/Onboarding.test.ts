import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import type { DesktopSnapshot, ProductRecord } from "./desktop-api";
import Onboarding from "./Onboarding.svelte";

vi.mock("./desktop-api", () => ({ productImageSrc: (product: ProductRecord) => product.imagePath }));
const confetti = vi.hoisted(() => ({ fire: vi.fn(), reset: vi.fn(), create: vi.fn() }));
vi.mock("canvas-confetti", () => ({ default: { create: confetti.create } }));
confetti.create.mockImplementation(() => Object.assign(confetti.fire, { reset: confetti.reset }));

afterEach(cleanup);

function product(productId: string, name: string, enabled = false): ProductRecord {
  return { productId, name, enabled, prominentAlert: false, checkCount: 0, observation: null, runtimeError: null,
    metadata: null, imagePath: null, metadataUpdatedAt: null, todayCheckCount: 0,
    todaySuccessCount: 0, todayFailureCount: 0, monitoringMs: 0 };
}

function mount(overrides: Partial<DesktopSnapshot> = {}, accepted = true) {
  const snapshot: DesktopSnapshot = {
    setupCompleted: false, systemNotificationsEnabled: false, systemNotificationDelivery: null,
    runtime: null, config: null, products: [],
    catalog: [product("19", "官翻品 GR III ING 套装版本"), product("50", "官翻品 RICOH GR III HDF"),
      product("65", "官翻品 GR IIIx"), product("66", "官翻品 RICOH GR III"),
      product("67", "官翻品 RICOH GR III 日记版"), product("114", "官翻品 GR IIIx HDF"),
      product("130", "官翻品 GR IV"), product("245", "官翻品 GR IV HDF"), product("108", "GR SPACE VIP会员卡"), product("9", "RICOH GR IIIx")],
    channels: [], providers: [
      { id: "feishu", name: "飞书", documentationUrl: null, fields: [] },
      { id: "wecom", name: "企业微信", documentationUrl: null, fields: [] },
      { id: "dingtalk", name: "钉钉", documentationUrl: null, fields: [] },
    ], proxies: [], scan: null, recentEvents: [], recentChecks: [],
    platform: { loginStartEnabled: false, notificationPermission: "prompt", notificationPermissionError: null, projectUrl: null, tutorialUrl: null, feedbackUrl: null },
    ...overrides,
  };
  const callbacks = {
    onProducts: vi.fn(async () => accepted), onSystemEnable: vi.fn(async () => {}),
    onSystemTest: vi.fn(async () => {}), onAddChannel: vi.fn(), onChannelTest: vi.fn(), onChannelToggle: vi.fn(),
    onComplete: vi.fn(async (_loginStart: boolean) => {}), onCancel: vi.fn(),
  };
  const view = render(Onboarding, { snapshot, ...callbacks });
  return { ...callbacks, snapshot, ...view };
}

async function next() {
  await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
  await waitFor(() => expect(screen.getByRole("heading", { name: "2/3 设置通知" })).toBeTruthy());
}

describe("三步首次设置", () => {
  it("完成页无步骤编号，自动监控默认开启并独立提交", async () => {
    const view = mount();
    await next();
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    expect(screen.getByRole("heading", { name: "恭喜！" })).toBeTruthy();
    expect(screen.queryByText("3/3")).toBeNull();
    const start = screen.getByRole("checkbox", { name: "启动后自动开启监控" }) as HTMLInputElement;
    expect(start.checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "登录后启动" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(start);
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(view.onComplete).toHaveBeenCalledWith(false, false);
  });
  it("跳过产品选择时使用默认集合，取消不会提交", async () => {
    const view = mount();
    await fireEvent.click(screen.getByRole("checkbox", { name: "官翻品 GR IIIx" }));
    await fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(view.onCancel).toHaveBeenCalledOnce();
    expect(view.onProducts).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    await screen.findByRole("heading", { name: "2/3 设置通知" });
    expect(view.onProducts).toHaveBeenCalledWith(["19", "50", "65", "66", "67", "114", "130", "245"]);
  });
  it("第一页仅显示官翻相机和会员卡，默认勾选八款相机", async () => {
    const view = mount();
    expect(screen.getAllByRole("checkbox").filter((input) => (input as HTMLInputElement).checked)).toHaveLength(8);
    expect(screen.getAllByRole("checkbox")).toHaveLength(9);
    expect(screen.queryByRole("checkbox", { name: "RICOH GR IIIx" })).toBeNull();
    const member = screen.getByRole("checkbox", { name: "GR SPACE VIP会员卡" }) as HTMLInputElement;
    expect(member.checked).toBe(false);
    await fireEvent.click(member);
    await next();
    expect(view.onProducts).toHaveBeenCalledWith(["19", "50", "65", "66", "67", "114", "130", "245", "108"]);
  });

  it("重跑保留已启用产品，额外已选产品仍可调整，目录与配置去重", async () => {
    const view = mount({ setupCompleted: true, products: [product("9", "RICOH GR IIIx", true), product("65", "官翻品 GR IIIx")] });
    expect(screen.getAllByRole("checkbox")).toHaveLength(10);
    expect((screen.getByRole("checkbox", { name: "RICOH GR IIIx" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "官翻品 GR IIIx" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(screen.getByRole("checkbox", { name: "RICOH GR IIIx" }));
    await view.rerender({ snapshot: { ...view.snapshot, products: [product("9", "RICOH GR IIIx"), product("65", "官翻品 GR IIIx")] } });
    expect(screen.getByRole("checkbox", { name: "RICOH GR IIIx" })).toBeTruthy();
  });

  it("未选择产品时无法下一步", async () => {
    const view = mount({ catalog: [product("65", "官翻品 GR IIIx")] });
    await fireEvent.click(screen.getByRole("checkbox"));
    expect((screen.getByRole("button", { name: "下一步" }) as HTMLButtonElement).disabled).toBe(true);
    expect(view.onProducts).not.toHaveBeenCalled();
  });

  it("产品保存失败留在第一步，保留选择并显示重试提示", async () => {
    const view = mount({}, false);
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("重试"));
    expect(screen.getByRole("heading", { name: "1/3 选择监控商品" })).toBeTruthy();
    expect(view.onProducts).toHaveBeenCalledOnce();
  });

  it("等待保存成功后才进入通知页", async () => {
    const view = mount();
    let resolve!: (accepted: boolean) => void;
    view.onProducts.mockImplementation(() => new Promise<boolean>((done) => { resolve = done; }));
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByRole("heading", { name: "1/3 选择监控商品" })).toBeTruthy();
    resolve(true);
    await waitFor(() => expect(screen.getByRole("heading", { name: "2/3 设置通知" })).toBeTruthy());
  });

  it("可跳过通知到完成页，保留必要通知提示并播放一次彩带", async () => {
    const view = mount();
    await next();
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    expect(screen.getByRole("heading", { name: "恭喜！" })).toBeTruthy();
    expect(screen.getByText(/App 内查看/)).toBeTruthy();
    expect(document.querySelector(".confetti[aria-hidden='true']")).toBeTruthy();
    const calls = confetti.create.mock.calls.length;
    await view.rerender({ snapshot: { ...view.snapshot, systemNotificationsEnabled: true } });
    expect(confetti.create.mock.calls).toHaveLength(calls);
  });

  it("返回第一步保留用户选择，刷新快照也不覆盖草稿", async () => {
    const view = mount();
    await fireEvent.click(screen.getByRole("checkbox", { name: "官翻品 GR IIIx" }));
    await next();
    await view.rerender({ snapshot: { ...view.snapshot, systemNotificationsEnabled: true } });
    await fireEvent.click(screen.getByRole("button", { name: "上一步" }));
    expect((screen.getByRole("checkbox", { name: "官翻品 GR IIIx" }) as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("checkbox", { name: "官翻品 GR IV" }) as HTMLInputElement).checked).toBe(true);
  });

  it("系统通知启用、测试和添加渠道接通父回调", async () => {
    const view = mount();
    await next();
    await fireEvent.click(screen.getByRole("button", { name: "启用系统通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "测试系统通知" }));
    await fireEvent.click(screen.getByRole("button", { name: "添加企业微信" }));
    expect(view.onSystemEnable).toHaveBeenCalledOnce();
    expect(view.onSystemTest).toHaveBeenCalledOnce();
    expect(view.onAddChannel).toHaveBeenCalledWith("wecom");
    expect(screen.getByText("飞书")).toBeTruthy();
    expect(screen.getByText("企业微信")).toBeTruthy();
    expect(screen.getByText("钉钉")).toBeTruthy();
  });

  it("现有通知渠道展示实际状态并可测试", async () => {
    const view = mount({ channels: [{ id: "bark-1", name: "我的手机", providerId: "bark", providerName: "Bark",
      enabled: true, configuredFieldKeys: ["server"], subscriptions: ["stock_available"],
      lastTest: { outcome: "accepted", message: "测试请求已接受" }, lastDelivery: null }] });
    await next();
    expect(screen.getByText("我的手机")).toBeTruthy();
    expect(screen.getByLabelText("已启用 · 连接已停止 · 测试成功")).toBeTruthy();
    await fireEvent.click(screen.getByRole("button", { name: "测试我的手机" }));
    expect(view.onChannelTest).toHaveBeenCalledWith("bark-1");
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.queryByText(/App 内查看/)).toBeNull();
  });

  it("测试请求被接受后，可启用尚未启用的通知渠道", async () => {
    const view = mount({ channels: [{ id: "bark-1", name: "我的手机", providerId: "bark", providerName: "Bark",
      enabled: false, configuredFieldKeys: ["server"], subscriptions: ["stock_available"],
      lastTest: { outcome: "accepted", message: "测试请求已接受" }, lastDelivery: null }] });
    await next();
    const enable = screen.getByRole("button", { name: "启用我的手机" }) as HTMLButtonElement;
    expect(enable.disabled).toBe(false);
    await fireEvent.click(enable);
    expect(view.onChannelToggle).toHaveBeenCalledWith("bark-1", true);
    await view.rerender({ snapshot: { ...view.snapshot, channels: [{ ...view.snapshot.channels[0], enabled: true }] } });
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.queryByText(/App 内查看/)).toBeNull();
  });

  it.each([null, "failed", "invalid", "not_configured", "unknown"] as const)(
    "渠道测试状态为 %s 时，启用按钮禁用并说明先测试", async (outcome) => {
      const view = mount({ channels: [{ id: "bark-1", name: "我的手机", providerId: "bark", providerName: "Bark",
        enabled: false, configuredFieldKeys: ["server"], subscriptions: ["stock_available"],
        lastTest: outcome ? { outcome, message: null } : null, lastDelivery: null }] });
      await next();
      const enable = screen.getByRole("button", { name: "启用我的手机" }) as HTMLButtonElement;
      expect(enable.disabled).toBe(true);
      expect(screen.getByText("先测试，再启用。")).toBeTruthy();
      enable.click();
      expect(view.onChannelToggle).not.toHaveBeenCalled();
    },
  );

  it("已启用的渠道可直接停用，即使最近测试失败", async () => {
    const view = mount({ channels: [{ id: "bark-1", name: "我的手机", providerId: "bark", providerName: "Bark",
      enabled: true, configuredFieldKeys: ["server"], subscriptions: ["stock_available"],
      lastTest: { outcome: "failed", message: "请求失败" }, lastDelivery: null }] });
    await next();
    const disable = screen.getByRole("button", { name: "停用我的手机" }) as HTMLButtonElement;
    expect(disable.disabled).toBe(false);
    await fireEvent.click(disable);
    expect(view.onChannelToggle).toHaveBeenCalledWith("bark-1", false);
  });

  it("重跑沿用监控计划，并在完成时提交启用自启动", async () => {
    const view = mount({ setupCompleted: true, products: [product("65", "官翻品 GR IIIx", true)],
      config: { autoStartMonitoring: true, monitoringMode: "listed_products", schedule: { days: ["mon", "fri"], start: "09:00", end: "19:00" },
        rate: { intervalMinMs: 1000, intervalMaxMs: 2000, failuresBeforeBackoff: 3, failureBackoffSeconds: 20 },
        useSystemProxy: false, useProxyPool: false, failureAlertAfterMinutes: 10 } });
    await next();
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    expect(screen.queryByText(/当前监控计划/)).toBeNull();
    await fireEvent.click(screen.getByRole("checkbox", { name: "登录后启动" }));
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(view.onComplete).toHaveBeenCalledWith(true, true);
  });

  it("登录后启动读取实际状态，完成时提交勾选值", async () => {
    const view = mount({ platform: { loginStartEnabled: true, notificationPermission: "granted", notificationPermissionError: null, projectUrl: null, tutorialUrl: null, feedbackUrl: null } });
    await next();
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    const loginStart = screen.getByRole("checkbox", { name: "登录后启动" }) as HTMLInputElement;
    expect(loginStart.checked).toBe(true);
    await fireEvent.click(loginStart);
    await fireEvent.click(screen.getByRole("button", { name: "完成" }));
    expect(view.onComplete).toHaveBeenCalledWith(false, true);
  });

  it("步骤标题、返回操作与完成文案对应当前步骤", async () => {
    mount({ setupCompleted: true, products: [product("65", "官翻品 GR IIIx", true)] });
    for (const text of ["open-richo-monitor", "1 / 3", "退出引导", "01 · 监控产品", "官翻 GR III / IV", "至少选择一款产品"]) {
      expect(screen.queryByText(text)).toBeNull();
    }
    expect(screen.queryByText(/已选.*款产品|其他官方产品|勾选你关注/)).toBeNull();
    expect(screen.queryByText(/产品 ID/)).toBeNull();
    expect(screen.getByRole("heading", { name: "1/3 选择监控商品" })).toBeTruthy();
    await next();
    expect(screen.queryByRole("button", { name: "退出引导" })).toBeNull();
    expect(screen.getByRole("button", { name: "上一步" })).toBeTruthy();
    expect(screen.queryByText(/02 ·|通知方式可以稍后|启用时可能需要/)).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "跳过" }));
    expect(screen.getByRole("heading", { name: "恭喜！" })).toBeTruthy();
    expect(screen.getByText("现在您拥有了自己的理光商城上架监控。")).toBeTruthy();
    expect(screen.getByText("通知尚未启用，可在 App 内查看监控消息。")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "上一步" })).toBeNull();
    expect(screen.queryByRole("button", { name: "退出引导" })).toBeNull();
    expect(screen.getAllByRole("button")).toHaveLength(1);
    expect(screen.queryByText(/03 ·|已选择.*款产品|每天全天检查|自动打开应用/)).toBeNull();
  });

  it("完成页使用独立画布的现成礼花效果，并在关闭时清理", async () => {
    confetti.create.mockClear(); confetti.fire.mockClear(); confetti.reset.mockClear();
    const view = mount({ systemNotificationsEnabled: true, platform: { loginStartEnabled: false, notificationPermission: "granted", notificationPermissionError: null, projectUrl: null, tutorialUrl: null, feedbackUrl: null } });
    await next();
    await fireEvent.click(screen.getByRole("button", { name: "下一步" }));
    expect(screen.getByText("商品上架或补货时，会立即提醒你。")).toBeTruthy();
    expect(confetti.create).toHaveBeenCalledWith(expect.any(HTMLCanvasElement), { resize: true, disableForReducedMotion: true });
    expect(confetti.fire).toHaveBeenCalledTimes(5);
    await view.unmount();
    expect(confetti.reset).toHaveBeenCalledOnce();
  });
});
