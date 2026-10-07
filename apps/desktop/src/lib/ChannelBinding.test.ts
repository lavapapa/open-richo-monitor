import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChannelBinding as Binding, ChannelTarget } from "./desktop-api";

const api = vi.hoisted(() => ({ beginChannelBinding: vi.fn(), beginChannelRebinding: vi.fn(), beginChannelEditing: vi.fn(), endChannelEditing: vi.fn(), detectChannelGroups: vi.fn(), detectBindingGroups: vi.fn(), channelBindingStatus: vi.fn(), cancelChannelBinding: vi.fn(), submitChannelBindingVerification: vi.fn(), openExternalUrl: vi.fn() }));
const qr = vi.hoisted(() => ({ toDataURL: vi.fn(async () => "data:image/png;base64,cXI=") }));
vi.mock("./desktop-api", () => ({ desktopApi: api }));
vi.mock("qrcode", () => ({ default: qr }));
import ChannelBinding from "./ChannelBinding.svelte";

const waiting: Binding = { id: "binding-1", provider: "feishu", status: "waiting", connectionStatus: "ready", qrUrl: "https://official.example/registration", targets: [] };
const groups: ChannelTarget[] = [{ id: "chat-1", kind: "chat", label: "摄影群" }, { id: "chat-2", kind: "chat", label: "库存群" }];
beforeEach(() => {
  vi.useFakeTimers();
  api.beginChannelBinding.mockReset().mockResolvedValue(waiting);
  api.beginChannelRebinding.mockReset().mockResolvedValue(waiting);
  api.beginChannelEditing.mockReset().mockResolvedValue({ message: null });
  api.endChannelEditing.mockReset().mockResolvedValue({ message: null });
  api.detectChannelGroups.mockReset().mockResolvedValue([]);
  api.detectBindingGroups.mockReset().mockResolvedValue([]);
  api.channelBindingStatus.mockReset().mockResolvedValue(waiting);
  api.submitChannelBindingVerification.mockReset();
  api.cancelChannelBinding.mockReset().mockResolvedValue({ message: null });
  api.openExternalUrl.mockReset().mockResolvedValue({ message: null });
  qr.toDataURL.mockClear();
});
afterEach(() => { cleanup(); vi.useRealTimers(); });
const flush = () => vi.advanceTimersByTimeAsync(0);

describe("通知扫码连接", () => {
  it("私信确认失败不显示配对成功，群聊确认独立呈现状态", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", privateMessageReceived: true, targets: [{ id: "owner", kind: "user", label: "创建人" }, groups[0]], pairingReplies: [
      { target: { id: "owner", kind: "user", label: "创建人" }, outcome: "failed", message: "平台拒绝发送" },
      { target: groups[0], outcome: "accepted", message: "平台已接受" },
    ] });
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    expect(screen.getByText("已收到私信，确认消息发送未成功")).toBeTruthy();
    expect(screen.queryByText("已收到私信，配对成功")).toBeNull();
    expect(screen.getByRole("img", { name: "群聊配对成功" })).toBeTruthy();
  });
  it("确认回复还在发送时呈现等待，收到平台回执后才显示配对成功", async () => {
    const target = { id: "owner", kind: "user", label: "创建人" };
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", privateMessageReceived: true, targets: [target], pairingReplies: [{ target, outcome: "sending" }] });
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    expect(screen.getByText("已收到私信，正在发送确认")).toBeTruthy();
    expect(screen.queryByText("已收到私信，配对成功")).toBeNull();
    api.channelBindingStatus.mockResolvedValue({ ...waiting, status: "complete", privateMessageReceived: true, targets: [target], pairingReplies: [{ target, outcome: "accepted" }] });
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByText("已收到私信，配对成功")).toBeTruthy();
  });
  it.each([
    ["weixin", "微信", "用微信app扫码，连接并发送任意私信后返回"],
    ["feishu", "飞书", "用飞书app扫码，创建机器人并打开应用后返回"],
    ["wecom", "企业微信", "用企业微信app扫码，创建机器人并发送任意私信后返回"],
    ["dingtalk", "钉钉", "用钉钉app扫码，创建机器人并发送任意私信后返回"],
  ])("%s 在二维码下方保留一条具体操作提示", async (providerId, providerName, hint) => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: providerId });
    const view = render(ChannelBinding, { providerId, providerName });
    await flush();
    expect(screen.getByText(hint).closest(".qr-stage")?.querySelector("img")).toBeTruthy();
    expect(screen.getByRole("button", { name: "刷新二维码" }).textContent).toBe("刷新");
    await view.rerender({ binding: { ...waiting, provider: providerId, status: "scanned" } });
    expect(screen.getAllByText(hint)).toHaveLength(1);
    expect(screen.queryByText("请在手机上确认。")).toBeNull();
  });

  it.each([
    ["feishu", "飞书", "把机器人加入群聊，点击刷新群聊"],
    ["wecom", "企业微信", "加入群聊后，在群内@它发任意消息"],
    ["dingtalk", "钉钉", "加入内部群后，在群内@它发任意消息"],
  ])("%s 群聊区使用平台对应的群聊步骤", async (providerId, providerName, hint) => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: providerId, status: "complete", targets: [{ id: "user", kind: "user", label: "扫码账号" }] });
    render(ChannelBinding, { providerId, providerName });
    await flush();
    expect(screen.getByText(hint).closest("section")?.querySelector("button")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "在群聊中发送（可选）" })).toBeTruthy();
    expect(screen.queryByText("接收位置")).toBeNull();
    expect(screen.queryByText("暂未识别到接收对象。")).toBeNull();
  });

  it("二维码可主动刷新，取消旧会话并显示新二维码", async () => {
    api.beginChannelBinding.mockResolvedValueOnce(waiting).mockResolvedValue({ ...waiting, id: "binding-2", qrUrl: "https://official.example/new-registration" });
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await fireEvent.click(screen.getByRole("button", { name: "刷新二维码" }));
    await flush();
    expect(api.cancelChannelBinding).toHaveBeenCalledWith("binding-1");
    expect(api.beginChannelBinding).toHaveBeenCalledTimes(2);
    expect(qr.toDataURL).toHaveBeenLastCalledWith("https://official.example/new-registration", expect.anything());
  });

  it("私聊默认勾选且与群聊分开，取消后轮询刷新不重新勾选", async () => {
    const complete: Binding = { ...waiting, status: "complete", targets: [{ id: "owner", kind: "user", label: "绑定账号" }, ...groups] };
    api.beginChannelBinding.mockResolvedValue(complete);
    api.channelBindingStatus.mockResolvedValue(complete);
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    const personal = screen.getByRole("checkbox", { name: "创建人 个人" }) as HTMLInputElement;
    expect(personal.checked).toBe(true);
    expect(personal.closest("fieldset")?.getAttribute("aria-label")).toBe("私聊接收对象");
    expect(screen.getByRole("checkbox", { name: "摄影群 群聊" }).closest("fieldset")?.getAttribute("aria-label")).toBe("群聊接收对象");
    await fireEvent.click(personal);
    await vi.advanceTimersByTimeAsync(2000);
    expect((screen.getByRole("checkbox", { name: "创建人 个人" }) as HTMLInputElement).checked).toBe(false);
    expect(screen.queryByRole("button", { name: /命名创建人|命名绑定账号/ })).toBeNull();
    expect(screen.getByText("手动填写接收对象").closest("details")?.open).toBe(false);
  });

  it("私聊保留平台姓名且不提供改名，编辑保留仅群聊选择", async () => {
    const owner: ChannelTarget = { id: "owner", kind: "user", label: "李一" };
    render(ChannelBinding, { providerId: "dingtalk", providerName: "钉钉", existing: { id: "stored", name: "相机", providerId: "dingtalk", providerName: "钉钉", enabled: true, connectionStatus: "ready", configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null, targets: [owner, ...groups] }, selectedTargets: [groups[0]] });
    await flush();
    expect(screen.getByLabelText("私聊接收对象").textContent).toContain("李一");
    expect(screen.queryByRole("button", { name: "命名李一" })).toBeNull();
    expect((screen.getByRole("checkbox", { name: "李一 个人" }) as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    expect((screen.getByRole("checkbox", { name: "李一 个人" }) as HTMLInputElement).checked).toBe(false);
    expect(api.beginChannelBinding).not.toHaveBeenCalled();
  });

  it("手机要求配对码时可提交，继续使用原扫码会话", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: "weixin", status: "needs_verification" });
    api.submitChannelBindingVerification.mockResolvedValue({ ...waiting, provider: "weixin", status: "scanned" });
    render(ChannelBinding, { providerId: "weixin", providerName: "微信" });
    await flush();
    expect(screen.getByText("输入手机配对码，确认后继续")).toBeTruthy();
    await fireEvent.input(screen.getByLabelText("手机配对码"), { target: { value: "123456" } });
    await fireEvent.click(screen.getByRole("button", { name: "确认配对" }));
    await flush();
    expect(api.submitChannelBindingVerification).toHaveBeenCalledWith("binding-1", "123456");
    expect(api.beginChannelBinding).toHaveBeenCalledTimes(1);
  });
  it("微信授权后显示连接状态并继续提示发送首条私信", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: "weixin", status: "complete", connectionStatus: "ready", targets: [{ id: "wx-user", kind: "user", label: "扫码账号" }] });
    render(ChannelBinding, { providerId: "weixin", providerName: "微信" });
    await flush();
    expect(screen.getByLabelText("私聊接收对象").textContent).toContain("扫码账号");
    expect(screen.queryByText(/识别会话后再发送测试/)).toBeNull();
    expect(screen.getByRole("img", { name: "已连接微信" })).toBeTruthy();
    expect(screen.getByText("请向机器人发送任意私信，再返回保存")).toBeTruthy();
  });
  it("钉钉保留机器人链接资料和初始化指引，默认隐藏跳转入口", async () => {
    const botUrl = "https://open-dev.dingtalk.com/fe/app#/corp/robot";
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: "dingtalk", status: "complete", connectionStatus: "ready", targets: [], botUrl, appName: "授权创建的应用名称" });
    render(ChannelBinding, { providerId: "dingtalk", providerName: "钉钉", defaultName: "钉钉 1" });
    await flush();
    expect(screen.getByText("请向机器人发送任意私信，再返回保存")).toBeTruthy();
    expect(screen.queryByText(/钉钉尚未返回机器人名称/)).toBeNull();
    expect(screen.queryByRole("button", { name: "查看钉钉机器人" })).toBeNull();
    expect((screen.getByText("查看钉钉机器人") as HTMLButtonElement).hidden).toBe(true);
    await fireEvent.click(screen.getByText("查看钉钉机器人"));
    expect(api.openExternalUrl).toHaveBeenCalledExactlyOnceWith(botUrl);
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  it("使用绑定的官方入口打开机器人，重新授权时不沿用旧机器人入口", async () => {
    const botUrl = "https://applink.feishu.cn/client/bot/open?appId=cli-bound";
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", connectionStatus: "ready", targets: [], botUrl });
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    expect(screen.queryByRole("button", { name: "打开机器人" })).toBeNull();
    await fireEvent.click(screen.getByText("打开机器人"));
    expect(api.openExternalUrl).toHaveBeenCalledExactlyOnceWith(botUrl);
    await view.rerender({ binding: { ...waiting, status: "complete", connectionStatus: "ready" }, existing: { id: "stored", name: "旧账号", providerId: "feishu", providerName: "飞书", enabled: false, configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null, botUrl } });
    expect(screen.queryByRole("button", { name: "打开机器人" })).toBeNull();
  });
  it("打开机器人失败仅重试链接，保持原绑定与接收对象", async () => {
    const botUrl = "https://applink.feishu.cn/client/bot/open?appId=cli-bound";
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", targets: [{ id: "user", kind: "user", label: "扫码账号" }], botUrl });
    api.openExternalUrl.mockRejectedValueOnce(new Error("无法打开链接"));
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await fireEvent.click(screen.getByText("打开机器人"));
    expect(screen.getByRole("alert").textContent).toContain("无法打开链接");
    expect(screen.queryByRole("button", { name: "重新获取二维码" })).toBeNull();
    await fireEvent.click(screen.getByText("打开机器人"));
    expect(screen.queryByRole("alert")).toBeNull();
    expect(api.beginChannelBinding).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText("私聊接收对象").textContent).toContain("扫码账号");
  });
  it("配对码确认后丢弃迟到的验证码轮询，保持已连接状态", async () => {
    let resolvePoll!: (value: Binding) => void;
    const verifying: Binding = { ...waiting, provider: "weixin", status: "needs_verification" };
    api.beginChannelBinding.mockResolvedValue(verifying);
    api.channelBindingStatus.mockReturnValue(new Promise<Binding>((done) => { resolvePoll = done; }));
    api.submitChannelBindingVerification.mockResolvedValue({ ...waiting, provider: "weixin", status: "complete", targets: [{ id: "wx-user", kind: "user", label: "扫码账号" }] });
    render(ChannelBinding, { providerId: "weixin", providerName: "微信" });
    await flush();
    await vi.advanceTimersByTimeAsync(2000);
    await fireEvent.input(screen.getByLabelText("手机配对码"), { target: { value: "123456" } });
    await fireEvent.click(screen.getByRole("button", { name: "确认配对" }));
    await flush();
    resolvePoll(verifying);
    await flush();
    expect(screen.getByRole("img", { name: "已连接微信" })).toBeTruthy();
    expect(screen.queryByLabelText("手机配对码")).toBeNull();
  });
  it("验证码提交响应中断后轮询确认完成，清除错误且保持原绑定", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: "weixin", status: "needs_verification" });
    api.submitChannelBindingVerification.mockRejectedValueOnce(new Error("提交响应中断"));
    api.channelBindingStatus.mockResolvedValue({ ...waiting, provider: "weixin", status: "complete", targets: [{ id: "wx-user", kind: "user", label: "扫码账号" }] });
    render(ChannelBinding, { providerId: "weixin", providerName: "微信" });
    await flush();
    await fireEvent.input(screen.getByLabelText("手机配对码"), { target: { value: "123456" } });
    await fireEvent.click(screen.getByRole("button", { name: "确认配对" }));
    await flush();
    expect(screen.getByRole("alert").textContent).toContain("提交响应中断");
    expect(screen.queryByRole("button", { name: "重新获取二维码" })).toBeNull();
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("img", { name: "已连接微信" })).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(api.beginChannelBinding).toHaveBeenCalledTimes(1);
  });
  it("状态轮询暂时失败后继续原会话，恢复后移除错误", async () => {
    api.channelBindingStatus.mockRejectedValueOnce(new Error("暂时无法读取状态")).mockResolvedValue(waiting);
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("alert").textContent).toContain("暂时无法读取状态");
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(api.beginChannelBinding).toHaveBeenCalledTimes(1);
    expect(api.channelBindingStatus).toHaveBeenCalledTimes(2);
  });
  it("扫码授权完成后按实际连接显示状态，有效私信进入默认接收对象", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: "dingtalk", status: "complete", connectionStatus: "connecting", targets: [] });
    render(ChannelBinding, { providerId: "dingtalk", providerName: "钉钉" });
    await flush();
    expect(screen.getByRole("img", { name: "钉钉连接中" }).querySelector(".connection-mark.ready")).toBeNull();
    expect(screen.getByRole("status").textContent).toContain("正在连接钉钉");
    api.channelBindingStatus.mockResolvedValue({ ...waiting, provider: "dingtalk", status: "complete", connectionStatus: "ready", targets: [], message: "已收到私信，钉钉未提供可用于通知的员工 ID。请使用机器人所属组织的账号发送私信，或在群内 @机器人后选择群聊。" });
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("img", { name: "已连接钉钉" })).toBeTruthy();
    expect(screen.getByRole("status").textContent).toContain("已收到私信");
    api.channelBindingStatus.mockResolvedValue({ ...waiting, provider: "dingtalk", status: "complete", connectionStatus: "ready", targets: [{ id: "staff-1", kind: "user", label: "成员" }] });
    await vi.advanceTimersByTimeAsync(2000);
    expect((screen.getByRole("checkbox", { name: "成员 个人" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.queryByText(/已收到私信/)).toBeNull();
    api.channelBindingStatus.mockResolvedValue({ ...waiting, provider: "dingtalk", status: "complete", connectionStatus: "failed", targets: [] });
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("img", { name: "钉钉连接未就绪" }).querySelector(".connection-mark.ready")).toBeNull();
  });
  it("扫码完成显示图标，名称按自定义和机器人名称取值，接收对象可命名", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", botName: "库存助手", targets: [{ id: "opaque-chat-id", kind: "chat", label: "opaque-chat-id" }] });
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书", defaultName: "飞书 2" });
    await flush();
    expect(screen.getByRole("img", { name: "已连接飞书" }).querySelector(".connection-mark")).toBeTruthy();
    expect((screen.getByLabelText("渠道名称") as HTMLInputElement).value).toBe("库存助手");
    expect(screen.queryByText("opaque-chat-id")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "编辑渠道名称" }));
    expect(screen.getByLabelText("渠道名称").hasAttribute("readonly")).toBe(false);
    await fireEvent.input(screen.getByLabelText("渠道名称"), { target: { value: "自定义助手" } });
    await view.rerender({ binding: { ...waiting, status: "complete", botName: "新机器人名", targets: [{ id: "opaque-chat-id", kind: "chat", label: "opaque-chat-id" }] } });
    expect((screen.getByLabelText("渠道名称") as HTMLInputElement).value).toBe("自定义助手");
    await fireEvent.click(screen.getByRole("button", { name: "命名群聊" }));
    await fireEvent.change(screen.getByLabelText("接收对象名称"), { target: { value: "理光测试群" } });
    expect(screen.getByRole("checkbox", { name: "理光测试群 群聊" })).toBeTruthy();
  });
  it("停用渠道编辑期间监听新群，关闭后释放临时连接", async () => {
    const stored = { id: "stored", name: "企微", providerId: "wecom", providerName: "企业微信", enabled: false, configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null, connectionStatus: "stopped", targets: [], selectedTargets: [] };
    const view = render(ChannelBinding, { providerId: "wecom", providerName: "企业微信", existing: stored });
    await flush();
    expect(api.beginChannelEditing).toHaveBeenCalledWith("stored");
    expect(api.beginChannelBinding).not.toHaveBeenCalled();
    expect(screen.getByRole("heading", { name: "在群聊中发送（可选）" }).closest("details")).toBeNull();
    api.detectChannelGroups.mockResolvedValue(groups);
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    await view.rerender({ existing: { ...stored, targets: [{ id: "new-group", kind: "chat", label: "新加入群" }] } });
    expect(screen.getByRole("checkbox", { name: "新加入群 群聊" })).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "摄影群 群聊" })).toBeTruthy();
    await view.rerender({ existing: null });
    view.unmount();
    await flush();
    expect(api.endChannelEditing).toHaveBeenCalledExactlyOnceWith("stored");
    expect(stored.enabled).toBe(false);
  });
  it("编辑连接尚未返回时关闭，完成后仍释放连接", async () => {
    let resolve!: (value: { message: null }) => void;
    api.beginChannelEditing.mockReturnValue(new Promise((done) => { resolve = done; }));
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书", existing: { id: "stored", name: "飞书", providerId: "feishu", providerName: "飞书", enabled: false, configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null } });
    view.unmount();
    resolve({ message: null });
    await flush();
    expect(api.endChannelEditing).toHaveBeenCalledExactlyOnceWith("stored");
  });
  it("刷新群失败保留连接与默认私信，重试成功清除群错误", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", targets: [{ id: "my-user", kind: "user", label: "我的账号" }] });
    api.detectBindingGroups.mockRejectedValueOnce(new Error("群权限不足")).mockResolvedValue(groups);
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    expect(screen.getByRole("alert").textContent).toContain("群权限不足");
    expect(screen.getByLabelText("通知接收对象").textContent).toContain("我的账号");
    expect(screen.queryByRole("button", { name: "重新获取二维码" })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    expect(screen.queryByRole("alert")).toBeNull();
  });
  it("获取二维码时显示转圈状态，接通后直接呈现私聊和群聊设置", async () => {
    let resolve!: (value: Binding) => void;
    api.beginChannelBinding.mockReturnValue(new Promise<Binding>((done) => { resolve = done; }));
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    expect(screen.getByRole("status", { name: "正在加载二维码" }).querySelector(".spinner")).toBeTruthy();
    resolve({ ...waiting, status: "complete", targets: [{ id: "my-user", kind: "user", label: "我的账号" }] });
    await flush();
    expect(screen.getByLabelText("通知接收对象").textContent).toContain("我的账号");
    expect(screen.queryByText("接收位置")).toBeNull();
    expect(screen.getByRole("checkbox", { name: "我的账号 个人" }).closest("details")).toBeNull();
    api.channelBindingStatus.mockResolvedValue({ ...waiting, status: "complete", targets: [{ id: "my-user", kind: "user", label: "我的账号" }, ...groups] });
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("checkbox", { name: "摄影群 群聊" }).closest("details")).toBeNull();
  });
  it("重扫新应用清除旧个人与群，恢复新会话默认选择并可检测新群", async () => {
    api.beginChannelBinding.mockResolvedValueOnce({ ...waiting, status: "complete", targets: [{ id: "old-user", kind: "user", label: "旧账号" }] }).mockResolvedValue({ ...waiting, id: "binding-2", status: "complete", targets: [{ id: "new-user", kind: "user", label: "新账号" }, { id: "new-chat", kind: "chat", label: "新群" }] });
    api.detectBindingGroups.mockResolvedValue(groups);
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    await fireEvent.click(screen.getByRole("checkbox", { name: "摄影群 群聊" }));
    expect((screen.getByRole("checkbox", { name: "旧账号 个人" }) as HTMLInputElement).checked).toBe(true);
    await view.component.restart();
    await flush();
    expect(screen.queryByRole("checkbox", { name: "旧账号 个人" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "摄影群 群聊" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "库存群 群聊" })).toBeNull();
    expect((screen.getByRole("checkbox", { name: "新账号 个人" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "新群 群聊" }) as HTMLInputElement).checked).toBe(false);
    expect(api.cancelChannelBinding).toHaveBeenCalledWith("binding-1");
  });

  it("既有渠道重新授权清空表单选择，取消保持持久配置", async () => {
    const stored = { id: "old", name: "摄影群", providerId: "feishu", providerName: "飞书", enabled: true, configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null, connectionStatus: "ready", targets: groups, selectedTargets: [groups[0]] };
    api.beginChannelRebinding.mockResolvedValue({ ...waiting, id: "rebind-1", status: "complete", targets: groups });
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书", existing: stored, selectedTargets: [{ ...groups[0] }], targetId: "old-manual" });
    await flush();
    await view.component.restart();
    await flush();
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(false);
    expect((screen.getByLabelText("接收对象 ID") as HTMLInputElement).value).toBe("");
    view.unmount();
    expect(stored.selectedTargets).toEqual([groups[0]]);
    expect(api.beginChannelRebinding).toHaveBeenCalledWith("old");
  });
  it("微信连接后等待首条私信，轮询识别后提示可保存且不显示手动接收对象", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, provider: "weixin", status: "complete", privateMessageReceived: false, targets: [{ id: "wx-user", kind: "user", label: "我的微信" }] });
    render(ChannelBinding, { providerId: "weixin", providerName: "微信" });
    await flush();
    expect(screen.queryByRole("button", { name: "刷新群聊" })).toBeNull();
    expect(screen.queryByText(/加入群聊|将机器人加入群聊/)).toBeNull();
    expect(screen.getByLabelText("通知接收对象").textContent).toContain("我的微信");
    expect(screen.queryByRole("checkbox", { name: "我的微信 个人" })).toBeNull();
    expect(screen.getByText("请向机器人发送任意私信，再返回保存")).toBeTruthy();
    expect(screen.queryByText("手动填写接收对象")).toBeNull();
    expect(screen.queryByRole("combobox", { name: "对象类型" })).toBeNull();
    api.channelBindingStatus.mockResolvedValue({ ...waiting, provider: "weixin", status: "complete", privateMessageReceived: true, targets: [{ id: "wx-user", kind: "user", label: "我的微信" }] });
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.queryByText("请向机器人发送任意私信，再返回保存")).toBeNull();
    expect(screen.getByText("已收到私信，配对成功")).toBeTruthy();
  });
  it("手动目标加入勾选列表，取消勾选后仍可重新选择", async () => {
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", targets: [] });
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await fireEvent.click(screen.getByText("手动填写接收对象"));
    await fireEvent.input(screen.getByLabelText("接收对象 ID"), { target: { value: "manual-chat" } });
    await fireEvent.click(screen.getByRole("button", { name: "添加接收对象" }));
    const checkbox = screen.getByRole("checkbox", { name: "群聊 群聊" }) as HTMLInputElement;
    expect(checkbox.checked).toBe(true);
    await fireEvent.click(checkbox);
    expect((screen.getByRole("checkbox", { name: "群聊 群聊" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(checkbox);
    expect(checkbox.checked).toBe(true);
  });
  it("检测两群后分别勾选，重新检测保留选择且不改动SDK返回数组", async () => {
    const initialTargets = [{ id: "user-1", kind: "user", label: "我的账号" }];
    api.beginChannelBinding.mockResolvedValue({ ...waiting, status: "complete", targets: initialTargets });
    api.detectBindingGroups.mockResolvedValue(groups);
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    expect(screen.getByRole("img", { name: "已连接飞书" })).toBeTruthy();
    expect((screen.getByRole("checkbox", { name: "我的账号 个人" }) as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    expect(api.detectBindingGroups).toHaveBeenCalledWith("binding-1");
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(false);
    expect((screen.getByRole("checkbox", { name: "库存群 群聊" }) as HTMLInputElement).checked).toBe(false);
    await fireEvent.click(screen.getByRole("checkbox", { name: "摄影群 群聊" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "库存群 群聊" }));
    await fireEvent.click(screen.getByRole("checkbox", { name: "我的账号 个人" }));
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "库存群 群聊" }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: "我的账号 个人" }) as HTMLInputElement).checked).toBe(false);
    expect(initialTargets).toEqual([{ id: "user-1", kind: "user", label: "我的账号" }]);
    expect(groups).toHaveLength(2);
    await fireEvent.click(screen.getByRole("checkbox", { name: "库存群 群聊" }));
    api.detectBindingGroups.mockResolvedValue([]);
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    await flush();
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(true);
    expect(screen.queryByRole("checkbox", { name: "库存群 群聊" })).toBeNull();
  });

  it("现有渠道检测保留已选群，关闭后旧检测结果不进入新表单", async () => {
    let resolve!: (value: ChannelTarget[]) => void;
    api.detectChannelGroups.mockReturnValue(new Promise<ChannelTarget[]>((done) => { resolve = done; }));
    const saved = { id: "old", name: "摄影群", providerId: "feishu", providerName: "飞书", enabled: true, configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null, connectionStatus: "ready", targets: groups, selectedTargets: [groups[0]] };
    const original = [{ ...groups[0] }];
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书", existing: saved, selectedTargets: original });
    await flush();
    expect(screen.getByRole("img", { name: "已连接飞书" })).toBeTruthy();
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(true);
    await fireEvent.click(screen.getByRole("button", { name: "刷新群聊" }));
    expect(api.detectChannelGroups).toHaveBeenCalledWith("old");
    view.unmount();
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    resolve([{ id: "stale", kind: "chat", label: "旧群" }]);
    await flush();
    expect(screen.queryByRole("checkbox", { name: "旧群 群聊" })).toBeNull();
    expect(original).toEqual([groups[0]]);
    expect(saved.selectedTargets).toEqual([groups[0]]);
  });
  it("在本地编码注册链接，扫码完成后持续发现收件目标", async () => {
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    expect(qr.toDataURL).toHaveBeenCalledWith(waiting.qrUrl, expect.objectContaining({ width: 232 }));
    expect(screen.getByAltText("使用飞书扫描二维码").getAttribute("src")).toMatch(/^data:/);
    api.channelBindingStatus.mockResolvedValueOnce({ ...waiting, status: "scanned" }).mockResolvedValueOnce({ ...waiting, status: "complete" }).mockResolvedValue({ ...waiting, status: "complete", targets: [{ id: "chat-1", kind: "chat", label: "摄影群" }] });
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("status").textContent).toContain("用飞书app扫码，创建机器人并打开应用后返回");
    await vi.advanceTimersByTimeAsync(2000);
    expect(screen.getByRole("status").textContent).toContain("请打开机器人应用，再返回保存");
    await vi.advanceTimersByTimeAsync(2000);
    expect((screen.getByRole("checkbox", { name: "摄影群 群聊" }) as HTMLInputElement).checked).toBe(false);
    expect(qr.toDataURL).toHaveBeenCalledOnce();
  });

  it("二维码过期后停止轮询并可获取新二维码", async () => {
    api.channelBindingStatus.mockResolvedValue({ ...waiting, status: "expired" });
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await vi.advanceTimersByTimeAsync(2000);
    const refresh = screen.getByRole("button", { name: "刷新二维码" });
    expect(refresh.closest(".qr-stage")).toBeTruthy();
    expect(refresh.textContent?.trim()).toBe("");
    expect(screen.queryByText(/二维码已过期/)).toBeNull();
    await vi.advanceTimersByTimeAsync(4000);
    expect(api.channelBindingStatus).toHaveBeenCalledOnce();
    await fireEvent.click(refresh);
    await flush();
    expect(api.cancelChannelBinding).toHaveBeenCalledWith("binding-1");
    expect(api.beginChannelBinding).toHaveBeenCalledTimes(2);
  });

  it("关闭组件取消当前会话，即使父级先清空绑定状态", async () => {
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    await flush();
    await view.rerender({ binding: null });
    view.unmount();
    await flush();
    expect(api.cancelChannelBinding).toHaveBeenCalledWith("binding-1");
    await vi.advanceTimersByTimeAsync(6000);
    expect(api.channelBindingStatus).not.toHaveBeenCalled();
  });

  it("关闭时尚未返回的二维码会话也会取消", async () => {
    let resolve!: (value: Binding) => void;
    api.beginChannelBinding.mockReturnValue(new Promise<Binding>((done) => { resolve = done; }));
    const view = render(ChannelBinding, { providerId: "feishu", providerName: "飞书" });
    view.unmount();
    resolve(waiting);
    await flush();
    expect(api.cancelChannelBinding).toHaveBeenCalledWith("binding-1");
    expect(qr.toDataURL).not.toHaveBeenCalled();
  });

  it("需要重新授权的渠道自动开启新二维码", async () => {
    render(ChannelBinding, { providerId: "feishu", providerName: "飞书", existing: { id: "old", name: "摄影群", providerId: "feishu", providerName: "飞书", enabled: false, configuredFieldKeys: [], subscriptions: [], lastTest: null, lastDelivery: null, connectionStatus: "auth_required" } });
    await flush();
    expect(api.beginChannelRebinding).toHaveBeenCalledWith("old");
    expect(api.beginChannelBinding).not.toHaveBeenCalled();
    expect(screen.getByAltText("使用飞书扫描二维码")).toBeTruthy();
  });
});
