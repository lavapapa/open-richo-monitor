import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, expect, it } from "vitest";
import type { NotificationChannel } from "./desktop-api";
import ChannelStatus from "./ChannelStatus.svelte";
import RecipientChip from "./RecipientChip.svelte";
afterEach(cleanup);
it("一个状态标签同时表达启用、连接、测试，连接和测试变化更新原位置", async () => {
  const channel: NotificationChannel = { id: "a", name: "助手", providerId: "weixin", providerName: "微信", enabled: true, configuredFieldKeys: [], subscriptions: [], connectionStatus: "ready", lastTest: { outcome: "accepted", message: null }, lastDelivery: null };
  const view = render(ChannelStatus, { channel });
  const tag = screen.getByLabelText("已启用 · 已连接 · 测试成功");
  expect(tag.querySelectorAll("svg")).toHaveLength(3);
  expect(tag.textContent?.trim()).toBe("");
  await view.rerender({ channel: { ...channel, connectionStatus: "reconnecting" }, testing: true });
  expect(screen.getByLabelText("已启用 · 重新连接中 · 正在测试")).toBe(tag);
  expect(tag.querySelectorAll(".spinning")).toHaveLength(2);
});

it("保留个人可识别的账号称呼，缺少群名时隐藏群 ID", () => {
  render(RecipientChip, { props: { target: { id: "QinHaiMing", kind: "user", label: "QinHaiMing" } } });
  expect(screen.getByText("QinHaiMing")).toBeTruthy();
  render(RecipientChip, { props: { target: { id: "opaque-group", kind: "chat", label: "opaque-group" } } });
  expect(screen.getByText("群聊")).toBeTruthy();
  expect(screen.queryByText("opaque-group")).toBeNull();
});
