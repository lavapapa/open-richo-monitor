import { beforeEach, describe, expect, it, vi } from "vitest";

const bridge = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(() => true),
  Channel: class { onmessage: (payload: unknown) => void = () => {}; },
}));

vi.mock("@tauri-apps/api/core", () => bridge);
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async (_name: string, _handler: (event: { payload: unknown }) => void) => vi.fn()) }));

import { desktopApi, formatMonitoringDuration, productImageSrc, sendSystemTestNotification } from "./desktop-api";

beforeEach(() => {
  bridge.invoke.mockReset();
  bridge.isTauri.mockReturnValue(true);
});

describe("系统测试通知命令", () => {
  it("保留命令层的实际错误，供界面呈现", async () => {
    bridge.invoke.mockRejectedValue("系统通知发送失败：通知服务暂不可用");
    await expect(sendSystemTestNotification()).rejects.toThrow("系统通知发送失败：通知服务暂不可用");
  });
  it.each(["accepted_by_system", "permission_denied"] as const)(
    "透传 %s，且只调用一次原生命令",
    async (outcome) => {
      bridge.invoke.mockResolvedValue(outcome);
      await expect(sendSystemTestNotification()).resolves.toBe(outcome);
      expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("test_system_notification", undefined);
    },
  );
});

describe("应用更新", () => {
  it("检查、安装并订阅更新事件", async () => {
    const status = { phase: "available", version: "0.2.0", notes: "修复问题", downloaded: 0, total: null, error: null } as const;
    bridge.invoke.mockResolvedValue(status);
    await expect(desktopApi.checkForUpdates()).resolves.toEqual(status);
    expect(bridge.invoke).toHaveBeenLastCalledWith("check_for_updates", undefined);
    await desktopApi.installUpdate();
    expect(bridge.invoke).toHaveBeenLastCalledWith("install_update");
    const receive = vi.fn();
    const stop = await desktopApi.subscribeUpdates(receive);
    const { listen } = await import("@tauri-apps/api/event");
    expect(listen).toHaveBeenCalledWith("desktop-update", expect.any(Function));
    stop();
  });

  it("预览模式返回 idle 且不请求原生命令", async () => {
    bridge.isTauri.mockReturnValue(false);
    await expect(desktopApi.checkForUpdates()).resolves.toMatchObject({ phase: "idle", error: null });
    await desktopApi.installUpdate();
    await desktopApi.subscribeUpdates(vi.fn());
    expect(bridge.invoke).not.toHaveBeenCalled();
  });
});

describe("引导与突出提醒命令", () => {
  it("自动监控偏好透传独立命令", async () => {
    bridge.invoke.mockResolvedValue({ message: null });
    await desktopApi.setAutoStartMonitoring(false);
    expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("set_auto_start_monitoring", { enabled: false });
  });
  it("扫码命令透传平台和绑定会话 ID，保存只提交绑定引用与接收对象", async () => {
    const binding = { id: "binding-1", provider: "feishu", status: "complete", targets: [] };
    bridge.invoke.mockResolvedValue(binding);
    await expect(desktopApi.beginChannelBinding("feishu")).resolves.toBe(binding);
    expect(bridge.invoke).toHaveBeenLastCalledWith("begin_channel_binding", { providerId: "feishu" });
    await desktopApi.channelBindingStatus(binding.id);
    expect(bridge.invoke).toHaveBeenLastCalledWith("channel_binding_status", { bindingId: binding.id });
    await desktopApi.detectBindingGroups(binding.id);
    expect(bridge.invoke).toHaveBeenLastCalledWith("detect_binding_groups", { bindingId: binding.id });
    await desktopApi.detectChannelGroups("channel-1");
    expect(bridge.invoke).toHaveBeenLastCalledWith("detect_notification_channel_groups", { channelId: "channel-1" });
    await desktopApi.beginChannelRebinding("channel-1");
    expect(bridge.invoke).toHaveBeenLastCalledWith("begin_channel_rebinding", { channelId: "channel-1" });
    const channel = { id: null, name: "摄影群", providerId: "feishu", bindingId: binding.id, values: { targetId: "chat-1", targetKind: "chat" }, subscriptions: ["stock_available" as const] };
    await desktopApi.saveChannel(channel);
    expect(bridge.invoke).toHaveBeenLastCalledWith("save_notification_channel", { channel });
    await desktopApi.cancelChannelBinding(binding.id);
    expect(bridge.invoke).toHaveBeenLastCalledWith("cancel_channel_binding", { bindingId: binding.id });
  });
  it("原子提交引导商品集合并透传结果", async () => {
    const result = { message: null };
    bridge.invoke.mockResolvedValue(result);
    await expect(desktopApi.setOnboardingProducts(["19", "130", "245"])).resolves.toBe(result);
    expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("set_onboarding_products", { productIds: ["19", "130", "245"] });
  });

  it.each([true, false])("透传商品突出提醒开关 %s", async (enabled) => {
    const result = { message: null };
    bridge.invoke.mockResolvedValue(result);
    await expect(desktopApi.setProductProminentAlert("130", enabled)).resolves.toBe(result);
    expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("set_product_prominent_alert", { productId: "130", enabled });
  });

  it("测试突出提醒透传商品编号", async () => {
    const result = { message: null };
    bridge.invoke.mockResolvedValue(result);
    await expect(desktopApi.testProminentAlert("245")).resolves.toBe(result);
    expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("test_prominent_alert", { productId: "245" });
  });

  it("注册直达提醒通道，提交内容后以当前事件显示", async () => {
    const receive = vi.fn();
    bridge.invoke.mockResolvedValue(undefined);
    const stop = await desktopApi.subscribeProminentAlert(receive);
    const args = bridge.invoke.mock.calls[0][1];
    expect(bridge.invoke.mock.calls[0][0]).toBe("subscribe_prominent_alert");
    const event = { eventId: 245, productId: "245", name: "官翻品 GR IV HDF", stock: 2 };
    args.channel.onmessage(event);
    expect(receive).toHaveBeenCalledExactlyOnceWith(event);
    await desktopApi.showProminentAlert(245, 2);
    expect(bridge.invoke).toHaveBeenLastCalledWith("show_prominent_alert", { eventId: 245, presentationId: 2 });
    stop();
    args.channel.onmessage(event);
    expect(receive).toHaveBeenCalledTimes(1);
  });

  it("关闭突出提醒透传对应事件编号", async () => {
    const result = { message: null };
    bridge.invoke.mockResolvedValue(result);
    await expect(desktopApi.dismissProminentAlert(245)).resolves.toBe(result);
    expect(bridge.invoke).toHaveBeenCalledExactlyOnceWith("dismiss_prominent_alert", { eventId: 245 });
  });
});

describe("累计监控时长", () => {
  it.each([
    [0, "0 分钟"],
    [5_400_000, "1 小时 30 分钟"],
    [92_630_000, "1 天 1 小时"],
  ])("%i 毫秒显示为 %s", (milliseconds, expected) => {
    expect(formatMonitoringDuration(milliseconds)).toBe(expected);
  });
});

describe("浏览器样本配置", () => {
  it("样本引导遵守自动监控偏好，设置开关保留当前监控", async () => {
    bridge.isTauri.mockReturnValue(false);
    const original = await desktopApi.getSnapshot();
    await desktopApi.setAutoStartMonitoring(false);
    expect((await desktopApi.getSnapshot()).runtime?.state).toBe(original.runtime?.state);
    await desktopApi.completeSetup();
    expect((await desktopApi.getSnapshot()).runtime?.state).toBe("stopped");
    await desktopApi.monitoringAction("start");
    expect((await desktopApi.getSnapshot()).runtime?.state).toBe("monitoring");
    await desktopApi.setAutoStartMonitoring(original.config!.autoStartMonitoring);
    expect(bridge.invoke).not.toHaveBeenCalled();
  });
  it("网页扫码预览只提供四个扫码平台，不调用原生命令", async () => {
    bridge.isTauri.mockReturnValue(false);
    const providers = (await desktopApi.getSnapshot()).providers;
    expect(providers.filter((provider) => provider.supportsBinding).map((provider) => provider.id)).toEqual(["feishu", "wecom", "dingtalk", "weixin"]);
    expect(providers.map((provider) => provider.id)).toEqual(["feishu", "wecom", "dingtalk", "weixin"]);
    const now = vi.spyOn(Date, "now").mockReturnValue(1000);
    try {
      const binding = await desktopApi.beginChannelBinding("feishu");
      expect(binding.status).toBe("waiting");
      now.mockReturnValue(6000);
      const completed = await desktopApi.channelBindingStatus(binding.id);
      expect(completed.status).toBe("complete");
      expect(completed.targets).toEqual([{ id: "preview-user", kind: "user", label: "演示账号" }]);
      expect(completed).not.toHaveProperty("appSecret");
      expect(completed).not.toHaveProperty("botToken");
      expect(await desktopApi.detectBindingGroups(binding.id)).toEqual([{ id: "preview-photo", kind: "chat", label: "摄影群" }, { id: "preview-duty", kind: "chat", label: "值班群" }]);
      expect((await desktopApi.channelBindingStatus(binding.id)).targets).toHaveLength(3);
      await desktopApi.cancelChannelBinding(binding.id);
      expect((await desktopApi.channelBindingStatus(binding.id)).status).toBe("cancelled");
      const weixin = await desktopApi.beginChannelBinding("weixin");
      await expect(desktopApi.detectBindingGroups(weixin.id)).rejects.toThrow();
      expect(bridge.invoke).not.toHaveBeenCalled();
    } finally { now.mockRestore(); }
  });
  it("网页引导可设置商品、通知、登录启动并完成，不调用原生命令", async () => {
    bridge.isTauri.mockReturnValue(false);
    const original = await desktopApi.getSnapshot();
    await desktopApi.setOnboardingProducts(["65", "130"]);
    expect((await desktopApi.getSnapshot()).products.filter((item) => item.enabled).map((item) => item.productId)).toEqual(["65", "130"]);
    await desktopApi.requestNotificationPermission();
    await desktopApi.setSystemNotificationsEnabled(true);
    await desktopApi.setLoginStart(true);
    await desktopApi.completeSetup();
    expect((await desktopApi.getSnapshot()).platform?.loginStartEnabled).toBe(true);
    expect((await desktopApi.getSnapshot()).setupCompleted).toBe(true);
    expect(bridge.invoke).not.toHaveBeenCalled();
    await desktopApi.setOnboardingProducts(original.products.filter((item) => item.enabled).map((item) => item.productId));
    await desktopApi.setLoginStart(original.platform?.loginStartEnabled ?? false);
  });
  it("未下载的图片先使用已有 image URL", async () => {
    bridge.isTauri.mockReturnValue(false);
    const product = (await desktopApi.getSnapshot()).products[0];
    expect(productImageSrc({ ...product, imagePath: null, metadata: { ...product.metadata!, imageUrl: "https://example.com/65.jpg" } })).toBe("https://example.com/65.jpg");
  });
  it("样本消息明确描述上架和库存改变", async () => {
    bridge.isTauri.mockReturnValue(false);
    const page = await desktopApi.queryMessages({ date: null, productId: null, cursor: null, limit: 100 });
    expect(page.items[0].detail).toBe("商品上架 · 补货 0 → 3");
    expect(page.items.every((item) => item.detail !== "状态变化")).toBe(true);
    expect(page.items[1].detail).toBe("检查结果：未上架 · 库存 0");
  });
  it("保存监控方案只更新样本快照，不调用桌面命令", async () => {
    bridge.isTauri.mockReturnValue(false);
    const original = (await desktopApi.getSnapshot()).config!;
    expect(original.monitoringMode).toBe("listed_products");
    await desktopApi.saveMonitoringConfig({ ...original, monitoringMode: "product_detail" });
    expect((await desktopApi.getSnapshot()).config?.monitoringMode).toBe("product_detail");
    await desktopApi.saveMonitoringConfig(original);
    expect(bridge.invoke).not.toHaveBeenCalled();
  });
});
