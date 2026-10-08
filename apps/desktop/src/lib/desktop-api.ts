import { Channel, convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";
import catalogPreview from "$lib/catalog-preview.json";

export type Weekday = "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";
export type RuntimeState =
  | "monitoring"
  | "partial_error"
  | "worker_failed"
  | "outside_schedule"
  | "paused"
  | "setup_incomplete"
  | "stopped";
export type AvailabilityState = "unknown" | "out_of_stock" | "in_stock" | "error";
export type ChannelEvent = "stock_available" | "monitoring_failed" | "recovered";

export interface UpdateStatus {
  phase: "idle" | "checking" | "available" | "downloading" | "waiting" | "installing";
  autoInstall: boolean;
  version: string | null;
  notes: string | null;
  downloaded: number;
  total: number | null;
  error: string | null;
}

export interface ScheduleConfig {
  days: Weekday[];
  start: string;
  end: string;
}

export interface RateConfig {
  intervalMinMs: number;
  intervalMaxMs: number;
  failuresBeforeBackoff: number;
  failureBackoffSeconds: number;
}

export interface MonitoringConfig {
  autoStartMonitoring: boolean;
  monitoringMode: "listed_products" | "product_detail";
  schedule: ScheduleConfig;
  rate: RateConfig;
  useSystemProxy: boolean;
  notificationUseSystemProxy: boolean;
  useProxyPool: boolean;
  failureAlertAfterMinutes: number;
}

export interface ProductObservation {
  availability: AvailabilityState;
  isShow: number;
  stock: number | null;
  checkedAt: string | null;
}

export interface ProductRecord {
  productId: string;
  name: string;
  enabled: boolean;
  prominentAlert: boolean;
  checkCount: number;
  observation: ProductObservation | null;
  runtimeError: string | null;
  metadata: ProductMetadata | null;
  imagePath: string | null;
  metadataUpdatedAt: string | null;
  todayCheckCount: number;
  todaySuccessCount: number;
  todayFailureCount: number;
  monitoringMs: number;
}

export interface ProductMetadata {
  imageUrl: string | null;
  galleryUrls: string[];
  price: string | null;
  unitName: string | null;
  productNo: string | null;
  isMemberCard: boolean;
  memberCardDays: number | null;
}

export interface ProminentAlert {
  eventId: number;
  productId: string;
  name: string;
  imageUrl: string | null;
  imagePath: string | null;
  price: string | null;
  stock: number;
  at: string;
}

export function prominentImageSrc(alert: ProminentAlert): string | null {
  if (!alert.imagePath) return null;
  return isTauri() ? convertFileSrc(alert.imagePath) : alert.imagePath;
}

export function productImageSrc(product: ProductRecord): string | null {
  if (!product.imagePath) return product.metadata?.imageUrl ?? null;
  return isTauri() ? convertFileSrc(product.imagePath) : product.imagePath;
}

export function formatMonitoringDuration(milliseconds: number): string {
  const totalMinutes = Math.floor(Math.max(0, milliseconds) / 60_000);
  const days = Math.floor(totalMinutes / 1_440);
  const hours = Math.floor(totalMinutes % 1_440 / 60);
  const minutes = totalMinutes % 60;
  if (days) return `${days} 天${hours ? ` ${hours} 小时` : ""}`;
  if (hours) return `${hours} 小时${minutes ? ` ${minutes} 分钟` : ""}`;
  return `${minutes} 分钟`;
}

export interface RuntimeSnapshot {
  state: RuntimeState;
  nextStartAt: string | null;
  lastSuccessAt: string | null;
  lastError: string | null;
}

export interface ChannelTest {
  outcome: "accepted" | "failed" | "invalid" | "not_configured" | "unknown";
  message: string | null;
}

export interface ChannelDelivery {
  outcome: "accepted" | "failed" | "unknown";
  event: ChannelEvent | "test";
  at: string;
  message: string;
}

export interface NotificationChannel {
  id: string;
  name: string;
  botName?: string | null;
  botUrl?: string | null;
  appName?: string | null;
  providerId: string;
  providerName: string;
  enabled: boolean;
  configuredFieldKeys: string[];
  subscriptions: ChannelEvent[];
  lastTest: ChannelTest | null;
  lastDelivery: ChannelDelivery | null;
  connectionStatus?: string;
  targets?: ChannelTarget[];
  selectedTargets?: ChannelTarget[];
  recipientDeliveries?: { target: ChannelTarget; lastDelivery: ChannelDelivery | null }[];
  targetId?: string | null;
  targetKind?: string | null;
}

export interface ChannelTarget { id: string; kind: string; label: string }
export interface ChannelBinding {
  id: string;
  provider: string;
  status: "waiting" | "scanned" | "needs_verification" | "complete" | "expired" | "cancelled" | "failed";
  connectionStatus?: string | null;
  qrUrl?: string | null;
  message?: string | null;
  targets: ChannelTarget[];
  botName?: string | null;
  botUrl?: string | null;
  appName?: string | null;
  privateMessageReceived?: boolean | null;
  privateChatReady?: boolean | null;
  pairingReplies?: { target: ChannelTarget; outcome: string; message?: string | null }[];
}

export interface ProviderField {
  key: string;
  label: string;
  type: "text" | "url" | "password" | "textarea";
  required: boolean;
  placeholder: string | null;
  help: string | null;
}

export interface ProviderDefinition {
  id: string;
  name: string;
  documentationUrl: string | null;
  fields: ProviderField[];
  supportsBinding?: boolean;
}

export interface ProxyRecord {
  id: string;
  protocol: "http" | "https" | "socks5";
  displayAddress: string;
  enabled: boolean;
  status: "untested" | "available" | "cooldown" | "auto_disabled" | "manually_disabled";
}

export interface ProductScan {
  id: string;
  startId: string;
  endId: string;
  currentId: string | null;
  checked: number;
  found: number;
  status: "running" | "paused" | "completed" | "cancelled" | "failed";
  error: string | null;
  results: ProductRecord[];
}

export interface PlatformSnapshot {
  loginStartEnabled: boolean | null;
  notificationPermission: "granted" | "denied" | "prompt" | "prompt_with_rationale" | "unavailable";
  notificationPermissionError: string | null;
  notificationSettingsAvailable: boolean;
  projectUrl: string | null;
  tutorialUrl: string | null;
  feedbackUrl: string | null;
}

export interface RecentEvent {
  id: number;
  at: string;
  kind: "info" | "warning" | "error" | "stock_available" | "stock_increased" | "monitoring_failed" | "recovered";
  productId: string;
  message: string;
}

export interface RecentCheck {
  productId: string;
  name: string;
  availability: "in_stock" | "out_of_stock";
  isShow: number;
  stock: number | null;
  at: string;
}

export interface MessageCursor { atMs: number; source: number; id: number }
export interface MessageItem { at: string; productId: string; name: string; isShow: number | null; stock: number | null; detail: string }
export interface MessagePage { items: MessageItem[]; nextCursor: MessageCursor | null }
export interface MessageQuery { date: string | null; productId: string | null; cursor: MessageCursor | null; limit: number }

export interface DesktopSnapshot {
  setupCompleted: boolean;
  systemNotificationsEnabled: boolean;
  systemNotificationDelivery: ChannelDelivery | null;
  runtime: RuntimeSnapshot | null;
  config: MonitoringConfig | null;
  products: ProductRecord[];
  catalog: ProductRecord[];
  channels: NotificationChannel[];
  providers: ProviderDefinition[];
  proxies: ProxyRecord[];
  scan: ProductScan | null;
  platform: PlatformSnapshot | null;
  recentEvents: RecentEvent[];
  recentChecks: RecentCheck[];
}

export interface OperationResult {
  message: string | null;
}

export interface ProxyImportResult {
  added: string[];
  duplicates: number;
  invalid: { line: number; reason: string }[];
}

export class DesktopApiError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DesktopApiError";
  }
}

const previewSnapshot: DesktopSnapshot = {
  setupCompleted: true,
  systemNotificationsEnabled: true,
  systemNotificationDelivery: null,
  runtime: { state: "monitoring", nextStartAt: null, lastSuccessAt: "2026-10-06T10:42:18.284+08:00", lastError: null },
  config: {
    autoStartMonitoring: true,
    monitoringMode: "listed_products",
    schedule: { days: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"], start: "09:00", end: "19:00" },
    rate: { intervalMinMs: 1000, intervalMaxMs: 2000, failuresBeforeBackoff: 3, failureBackoffSeconds: 20 },
    useSystemProxy: false, notificationUseSystemProxy: true, useProxyPool: false, failureAlertAfterMinutes: 10,
  },
  products: [
    { productId: "65", name: "官翻品 GR IIIx", enabled: true, prominentAlert: false, checkCount: 16385, observation: { availability: "in_stock", isShow: 1, stock: 3, checkedAt: "2026-10-06T10:42:18.284+08:00" }, runtimeError: null, metadata: { imageUrl: null, galleryUrls: [], price: "6749.0", unitName: "台", productNo: "B15289", isMemberCard: false, memberCardDays: null }, imagePath: "/catalog/65.jpg", metadataUpdatedAt: "2026-10-06T10:42:18+08:00", todayCheckCount: 702, todaySuccessCount: 702, todayFailureCount: 0, monitoringMs: 92_630_000 },
    { productId: "130", name: "官翻品 GR IV", enabled: true, prominentAlert: false, checkCount: 14220, observation: { availability: "out_of_stock", isShow: 1, stock: 0, checkedAt: "2026-10-06T10:36:05.120+08:00" }, runtimeError: null, metadata: { imageUrl: null, galleryUrls: [], price: "8819.0", unitName: "台", productNo: "B1555", isMemberCard: false, memberCardDays: null }, imagePath: "/catalog/130.jpg", metadataUpdatedAt: "2026-10-06T10:36:05+08:00", todayCheckCount: 689, todaySuccessCount: 689, todayFailureCount: 0, monitoringMs: 81_200_000 },
    { productId: "38", name: "RICOH GR III Diary Edition 日记版", enabled: true, prominentAlert: false, checkCount: 12038, observation: { availability: "out_of_stock", isShow: 1, stock: 0, checkedAt: "2026-10-06T10:30:11.426+08:00" }, runtimeError: null, metadata: { imageUrl: null, galleryUrls: [], price: "6799.0", unitName: "台", productNo: "1252", isMemberCard: false, memberCardDays: null }, imagePath: "/catalog/38.jpg", metadataUpdatedAt: "2026-10-06T10:30:11+08:00", todayCheckCount: 701, todaySuccessCount: 701, todayFailureCount: 0, monitoringMs: 73_200_000 },
    { productId: "108", name: "GR SPACE VIP会员卡", enabled: true, prominentAlert: false, checkCount: 302, observation: { availability: "unknown", isShow: 0, stock: 0, checkedAt: "2026-10-06T10:20:41.902+08:00" }, runtimeError: "商品详情接口请求失败", metadata: { imageUrl: null, galleryUrls: [], price: "199.0", unitName: "张", productNo: "GR SPACE VIP", isMemberCard: true, memberCardDays: 365 }, imagePath: "/catalog/108.jpg", metadataUpdatedAt: "2026-10-06T10:20:41+08:00", todayCheckCount: 12, todaySuccessCount: 11, todayFailureCount: 1, monitoringMs: 5_400_000 },
  ],
  catalog: [
    ["9", "RICOH GR IIIx"], ["18", "RICOH GR IIIx Urban Edition 都市版"],
    ["19", "官翻品 GR III ING 套装版本"], ["38", "RICOH GR III Diary Edition 日记版"],
    ["45", "RICOH GRIIIx HDF 指环带套餐"], ["46", "RICOH GR III HDF 套餐"],
    ["47", "RICOH GR III 套装"], ["48", "RICOH GR III Street Edition 街拍版"],
    ["49", "RICOH GR III Street Edition 街拍版套装"], ["50", "官翻品 RICOH GR III HDF"],
    ["51", "RICOH GR IIIx 指环带套装"], ["52", "RICOH GR IIIx Urban  都市版套装"],
    ["66", "官翻品 RICOH GR III"], ["67", "官翻品 RICOH GR III 日记版"],
    ["108", "GR SPACE VIP会员卡"], ["114", "官翻品 GR IIIx HDF"],
    ["122", "RICOH GR IV 电池套装"], ["123", "RICOH GR IV HDF 电池充电器套装"],
    ["124", "RICOH GR IV Monochrome"], ["245", "官翻品 GR IV HDF"],
  ].map(([productId, name]) => ({ productId, name, enabled: false, prominentAlert: false, checkCount: 0, observation: null, runtimeError: null, metadata: null, imagePath: null, metadataUpdatedAt: null, todayCheckCount: 0, todaySuccessCount: 0, todayFailureCount: 0, monitoringMs: 0 })),
  channels: [
    { id: "preview-feishu", name: "库存提醒", providerId: "feishu", providerName: "飞书", enabled: true, configuredFieldKeys: ["appId", "appSecret", "targetId", "targetKind"], connectionStatus: "ready", targets: [{ id: "preview-photo", kind: "chat", label: "摄影群" }], targetId: "preview-photo", targetKind: "chat", subscriptions: ["stock_available", "monitoring_failed"], lastTest: { outcome: "accepted", message: "测试通过" }, lastDelivery: { outcome: "accepted", event: "stock_available", at: "2026-09-29T09:10:00+08:00", message: "平台已接受" } },
    { id: "preview-wecom", name: "值班群", providerId: "wecom", providerName: "企业微信", enabled: false, configuredFieldKeys: ["botId", "secret", "targetId", "targetKind"], connectionStatus: "ready", targets: [{ id: "preview-duty", kind: "chat", label: "值班群" }], targetId: "preview-duty", targetKind: "chat", subscriptions: ["stock_available"], lastTest: null, lastDelivery: null },
    { id: "preview-dingtalk", name: "设备组", providerId: "dingtalk", providerName: "钉钉", enabled: false, configuredFieldKeys: ["appId", "appSecret", "targetId", "targetKind"], connectionStatus: "auth_required", targets: [], targetId: "preview-device", targetKind: "chat", subscriptions: ["recovered"], lastTest: { outcome: "failed", message: "需要重新授权" }, lastDelivery: null },
  ],
  providers: [
    { id: "feishu", name: "飞书", supportsBinding: true, documentationUrl: null, fields: credentialFields([["appId", "应用 ID"], ["appSecret", "应用密钥"]]) },
    { id: "wecom", name: "企业微信", supportsBinding: true, documentationUrl: null, fields: credentialFields([["botId", "机器人 ID"], ["secret", "机器人密钥"]]) },
    { id: "dingtalk", name: "钉钉", supportsBinding: true, documentationUrl: null, fields: credentialFields([["appId", "应用 ID"], ["appSecret", "应用密钥"]]) },
    { id: "weixin", name: "微信", supportsBinding: true, documentationUrl: null, fields: credentialFields([["botToken", "机器人令牌"], ["baseUrl", "服务地址", false], ["userId", "用户 ID", false], ["contextToken", "会话令牌", false]]) },
  ],
  proxies: [],
  scan: null,
  platform: { loginStartEnabled: false, notificationPermission: "granted", notificationPermissionError: null, notificationSettingsAvailable: false, projectUrl: null, tutorialUrl: null, feedbackUrl: null },
  recentEvents: [{ id: 1, at: "2026-10-04T09:18:00.284+08:00", kind: "stock_available", productId: "65", message: "商品 65 有货" }],
  recentChecks: [
    { productId: "65", name: "GR IIIx", availability: "in_stock", isShow: 1, stock: 3, at: "2026-10-04T09:18:00.284+08:00" },
    { productId: "130", name: "GR IV", availability: "out_of_stock", isShow: 0, stock: 0, at: "2026-10-04T09:10:00.613+08:00" },
    { productId: "65", name: "GR IIIx", availability: "out_of_stock", isShow: 0, stock: 0, at: "2026-10-04T09:00:00.128+08:00" },
  ],
};

function credentialFields(credentials: [string, string, boolean?][]): ProviderField[] {
  return [...credentials.map(([key, label, required = true]) => ({ key, label, required, type: (key === "baseUrl" ? "text" : "password") as ProviderField["type"], placeholder: null, help: null })), ...["targetId", "targetKind"].map((key) => ({ key, label: key === "targetId" ? "接收会话 ID" : "接收会话类型", type: "text" as const, required: true, placeholder: null, help: null }))];
}

for (const product of [...previewSnapshot.catalog, ...previewSnapshot.products]) {
  const cached = (catalogPreview as Record<string, { metadata: ProductMetadata; imagePath: string }>)[product.productId];
  if (cached) { product.metadata = cached.metadata; product.imagePath = cached.imagePath; }
}
for (const channel of previewSnapshot.channels) {
  channel.selectedTargets = channel.targetId ? [{ id: channel.targetId, kind: channel.targetKind ?? "chat", label: channel.targets?.find((target) => target.id === channel.targetId)?.label ?? channel.name }] : [];
}

const isPreview = () => import.meta.env.DEV && !isTauri();
const previewBindings = new Map<string, ChannelBinding & { startedAt: number }>();

function previewSetProductEnabled(productId: string, enabled: boolean): OperationResult {
  let product = previewSnapshot.products.find((item) => item.productId === productId);
  if (!product) {
    const catalogEntry = previewSnapshot.catalog.find((item) => item.productId === productId);
    if (!catalogEntry) throw new DesktopApiError("预览样本中没有此商品。");
    product = structuredClone(catalogEntry);
    previewSnapshot.products.push(product);
  }
  product.enabled = enabled;
  return { message: null };
}

function previewAddProduct(productId: string): ProductRecord {
  previewSetProductEnabled(productId, true);
  return structuredClone(previewSnapshot.products.find((item) => item.productId === productId)!);
}

function previewMonitoringAction(action: "start" | "pause" | "resume" | "restart"): OperationResult {
  previewSnapshot.runtime!.state = action === "pause" ? "paused" : "monitoring";
  return { message: null };
}

function previewSaveConfig(config: MonitoringConfig): OperationResult {
  previewSnapshot.config = structuredClone(config);
  return { message: null };
}

function previewMessagePage(query: MessageQuery): MessagePage {
  const checks = previewSnapshot.recentChecks;
  const recorded = new Set(checks.map((item) => `${item.productId}|${item.at}`));
  const items: MessageItem[] = [
    ...checks.map((item) => {
      const previous = checks.filter((before) => before.productId === item.productId && Date.parse(before.at) < Date.parse(item.at)).sort((a, b) => Date.parse(b.at) - Date.parse(a.at))[0];
      const changes: string[] = [];
      if (previous && previous.isShow !== item.isShow) changes.push(item.isShow === 1 ? "商品上架" : "商品下架");
      if (previous && previous.stock !== item.stock) changes.push(previous.stock === null ? `库存已获取：${item.stock}` : item.stock === null ? "接口未提供库存" : `${item.stock > previous.stock ? "补货" : "库存减少"} ${previous.stock} → ${item.stock}`);
      const detail = changes.length ? changes.join(" · ") : `检查结果：${item.isShow === 1 ? "已上架" : "未上架"}${item.stock === null ? "" : ` · 库存 ${item.stock}`}`;
      return { at: item.at, productId: item.productId, name: item.name, isShow: item.isShow, stock: item.stock, detail };
    }),
    ...previewSnapshot.recentEvents.filter((item) => item.kind !== "stock_available" || !recorded.has(`${item.productId}|${item.at}`)).map((item) => ({ at: item.at, productId: item.productId, name: previewSnapshot.products.find((product) => product.productId === item.productId)?.name ?? `商品 ${item.productId}`, isShow: null, stock: null, detail: item.kind === "monitoring_failed" ? "持续检查失败" : item.kind === "recovered" ? "检查恢复正常" : "商品有货" })),
  ];
  return { items: items.filter((item) => (!query.productId || item.productId === query.productId) && (!query.date || new Date(Date.parse(item.at) + 8 * 3600_000).toISOString().slice(0, 10) === query.date)).sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, query.limit), nextCursor: null };
}

function requireDesktop(): void {
  if (!isTauri()) {
    throw new DesktopApiError("请在桌面应用中使用此功能。");
  }
}

function previewCommand(name: string, payload: Record<string, any> = {}): unknown {
  const result = { message: null };
  switch (name) {
    case "begin_channel_binding": {
      const id = `preview-binding-${Date.now()}`;
      const binding = { id, provider: payload.providerId, status: "waiting" as const, qrUrl: `https://example.test/notification-preview/${id}`, targets: [], message: "网页演示二维码，请在桌面应用中连接。", startedAt: Date.now() };
      previewBindings.set(id, binding);
      return structuredClone(binding);
    }
    case "channel_binding_status": {
      const binding = previewBindings.get(payload.bindingId);
      if (!binding) throw new DesktopApiError("绑定会话已结束，请重新扫码。");
      if (binding.status === "waiting" && Date.now() - binding.startedAt > 4000) {
        binding.status = "complete";
        binding.connectionStatus = "ready";
        binding.privateMessageReceived = binding.provider === "weixin" ? true : null;
        binding.privateChatReady = true;
        binding.targets = [{ id: "preview-user", kind: "user", label: "演示账号" }];
        binding.message = "网页演示已连接，未注册真实机器人。";
      }
      return structuredClone(binding);
    }
    case "cancel_channel_binding": {
      const binding = previewBindings.get(payload.bindingId);
      if (binding) binding.status = "cancelled";
      return result;
    }
    case "begin_channel_rebinding": {
      const channel = previewSnapshot.channels.find((item) => item.id === payload.channelId);
      if (!channel) throw new DesktopApiError("通知渠道已移除。");
      return previewCommand("begin_channel_binding", { providerId: channel.providerId });
    }
    case "detect_binding_groups": {
      const binding = previewBindings.get(payload.bindingId);
      if (!binding || binding.status !== "complete") throw new DesktopApiError("请先扫码完成授权。");
      if (binding.provider === "weixin") throw new DesktopApiError("微信仅支持个人会话。");
      const groups = [{ id: "preview-photo", kind: "chat", label: "摄影群" }, { id: "preview-duty", kind: "chat", label: "值班群" }];
      binding.targets = [...binding.targets.filter((target) => target.kind !== "chat"), ...groups];
      return structuredClone(groups);
    }
    case "detect_notification_channel_groups": {
      const channel = previewSnapshot.channels.find((item) => item.id === payload.channelId);
      if (!channel) throw new DesktopApiError("通知渠道已移除。");
      const groups = [{ id: "preview-photo", kind: "chat", label: "摄影群" }, { id: "preview-duty", kind: "chat", label: "值班群" }];
      channel.targets = [...(channel.targets ?? []).filter((target) => target.kind !== "chat"), ...groups];
      return structuredClone(groups);
    }
    case "begin_channel_editing":
    case "end_channel_editing": return result;
    case "set_onboarding_products": {
      const all = new Map([...previewSnapshot.catalog, ...previewSnapshot.products].map((product) => [product.productId, product]));
      if (!payload.productIds.length || payload.productIds.some((id: string) => !all.has(id))) throw new DesktopApiError("请选择样本中的商品。");
      previewSnapshot.products = payload.productIds.map((id: string) => ({ ...all.get(id)!, enabled: true }));
      previewSnapshot.catalog = [...all.values()].filter((product) => !payload.productIds.includes(product.productId)).map((product) => ({ ...product, enabled: false }));
      return result;
    }
    case "set_product_prominent_alert": {
      const product = previewSnapshot.products.find((item) => item.productId === payload.productId);
      if (!product?.enabled) throw new DesktopApiError("请先启用商品监控。");
      product.prominentAlert = payload.enabled;
      return result;
    }
    case "set_auto_start_monitoring": previewSnapshot.config!.autoStartMonitoring = payload.enabled; return result;
    case "set_notification_use_system_proxy": previewSnapshot.config!.notificationUseSystemProxy = payload.enabled; return result;
    case "set_system_notifications_enabled": previewSnapshot.systemNotificationsEnabled = payload.enabled; return result;
    case "request_notification_permission": previewSnapshot.platform!.notificationPermission = "granted"; return structuredClone(previewSnapshot.platform);
    case "refresh_notification_permission": return structuredClone(previewSnapshot.platform);
    case "set_login_start": previewSnapshot.platform!.loginStartEnabled = payload.enabled; return structuredClone(previewSnapshot.platform);
    case "complete_setup": previewSnapshot.setupCompleted = true; previewSnapshot.runtime!.state = previewSnapshot.config!.autoStartMonitoring ? "monitoring" : "stopped"; return result;
    case "test_system_notification": return "accepted_by_system";
    case "save_notification_channel": {
      const form = payload.channel;
      const provider = previewSnapshot.providers.find((item) => item.id === form.providerId)!;
      const old = previewSnapshot.channels.find((item) => item.id === form.id);
      const binding = form.bindingId ? previewBindings.get(form.bindingId) : null;
      const selectedTargets = form.targets ?? old?.selectedTargets ?? [];
      const credentialsChanged = !old || !!binding || Object.entries(form.values).some(([key, value]) => !key.startsWith("target") && String(value).trim());
      const saved: NotificationChannel = { id: old?.id ?? `preview-${Date.now()}`, name: form.name, botName: binding?.botName ?? old?.botName, botUrl: binding?.botUrl ?? old?.botUrl, appName: binding?.appName ?? old?.appName, providerId: provider.id, providerName: provider.name, enabled: credentialsChanged ? false : old!.enabled, configuredFieldKeys: [...new Set([...(old?.configuredFieldKeys ?? []), ...Object.keys(form.values)])], subscriptions: form.subscriptions, lastTest: credentialsChanged ? null : old!.lastTest, lastDelivery: old?.lastDelivery ?? null, connectionStatus: "ready", targets: binding?.targets ?? old?.targets ?? [], selectedTargets: structuredClone(selectedTargets), targetId: selectedTargets[0]?.id ?? form.values.targetId ?? old?.targetId, targetKind: selectedTargets[0]?.kind ?? form.values.targetKind ?? old?.targetKind };
      previewSnapshot.channels = [...previewSnapshot.channels.filter((item) => item.id !== saved.id), saved];
      return structuredClone(saved);
    }
    case "test_notification_channel": {
      const channel = previewSnapshot.channels.find((item) => item.id === payload.channelId)!;
      channel.lastTest = { outcome: "accepted", message: "网页演示，未发送真实通知。" };
      return channel.lastTest;
    }
    case "set_notification_channel_enabled": previewSnapshot.channels.find((item) => item.id === payload.channelId)!.enabled = payload.enabled; return result;
    case "test_prominent_alert": {
      const product = [...previewSnapshot.products, ...previewSnapshot.catalog].find((item) => item.productId === payload.productId)!;
      const alert: ProminentAlert = { eventId: -1, productId: product.productId, name: product.name, imagePath: product.imagePath, imageUrl: product.metadata?.imageUrl ?? null, price: product.metadata?.price ?? null, stock: 3, at: new Date().toISOString() };
      sessionStorage.setItem("rm.preview.alert", JSON.stringify(alert));
      return result;
    }
    case "get_prominent_alert": return JSON.parse(sessionStorage.getItem("rm.preview.alert") ?? "null");
    case "dismiss_prominent_alert": sessionStorage.removeItem("rm.preview.alert"); return result;
    default: throw new DesktopApiError("网页预览暂未提供此操作。");
  }
}

/** 后端命令以 Result<T, String> 表达失败。 */
async function command<T>(name: string, payload?: Record<string, unknown>): Promise<T> {
  if (isPreview()) return previewCommand(name, payload) as T;
  requireDesktop();
  try {
    return await invoke<T>(name, payload);
  } catch (error) {
    const detail = backendErrorMessage(error);
    throw new DesktopApiError(detail ?? "桌面后台未完成此操作，配置或状态尚未确认。");
  }
}

function backendErrorMessage(error: unknown): string | null {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.trim()) return message;
  }
  return null;
}

export const desktopApi = {
  get previewMode() { return isPreview(); },
  getSnapshot: () => isPreview()
    ? Promise.resolve(structuredClone(previewSnapshot))
    : command<DesktopSnapshot>("get_desktop_snapshot"),
  queryMessages: (query: MessageQuery) => isPreview()
    ? Promise.resolve(previewMessagePage(query))
    : command<MessagePage>("query_messages", { query }),
  refreshCatalog: () => isPreview()
    ? Promise.resolve({ message: null } as OperationResult)
    : command<OperationResult>("refresh_catalog_metadata"),

  subscribe: (handler: (snapshot: DesktopSnapshot) => void): Promise<UnlistenFn> => {
    if (isPreview()) return Promise.resolve(() => {});
    requireDesktop();
    return listen<DesktopSnapshot>("desktop-state-changed", ({ payload }) => handler(payload));
  },

  subscribeErrors: (handler: (error: string) => void): Promise<UnlistenFn> => {
    if (isPreview()) return Promise.resolve(() => {});
    requireDesktop();
    return listen<string>("desktop-error", ({ payload }) => handler(payload));
  },

  subscribeUpdates: (handler: (status: UpdateStatus) => void): Promise<UnlistenFn> => {
    if (isPreview()) return Promise.resolve(() => {});
    requireDesktop();
    return listen<UpdateStatus>("desktop-update", ({ payload }) => handler(payload));
  },
  subscribePurchase: (handler: () => void): Promise<UnlistenFn> => {
    if (isPreview()) return Promise.resolve(() => {});
    requireDesktop();
    return listen("purchase-completed", handler);
  },
  completePurchase: (eventId: number) => command<OperationResult>("complete_purchase", { eventId }),
  submitFeedback: (content: string) => command<void>("submit_feedback", { content }),
  checkForUpdates: (): Promise<UpdateStatus> => isPreview()
    ? Promise.resolve({ phase: "idle", autoInstall: true, version: null, notes: null, downloaded: 0, total: null, error: null })
    : command<UpdateStatus>("check_for_updates"),
  setAutoInstallUpdates: (enabled: boolean): Promise<UpdateStatus> => isPreview()
    ? Promise.resolve({ phase: "idle", autoInstall: enabled, version: null, notes: null, downloaded: 0, total: null, error: null })
    : invoke<UpdateStatus>("set_auto_install_updates", { enabled }),
  installUpdate: (immediate = false): Promise<void> => isPreview()
    ? Promise.resolve()
    : invoke<void>("install_update", { immediate }),

  validateProduct: (productId: string) =>
    command<ProductRecord>("validate_product", { productId }),
  testChannel: (channelId: string) =>
    command<ChannelTest>("test_notification_channel", { channelId }),
  beginChannelBinding: (providerId: string) =>
    command<ChannelBinding>("begin_channel_binding", { providerId }),
  beginChannelEditing: (channelId: string) => command<OperationResult>("begin_channel_editing", { channelId }),
  endChannelEditing: (channelId: string) => command<OperationResult>("end_channel_editing", { channelId }),
  channelBindingStatus: (bindingId: string) =>
    command<ChannelBinding>("channel_binding_status", { bindingId }),
  cancelChannelBinding: (bindingId: string) =>
    command<OperationResult>("cancel_channel_binding", { bindingId }),
  submitChannelBindingVerification: (bindingId: string, code: string) =>
    command<ChannelBinding>("submit_channel_binding_verification", { bindingId, code }),
  beginChannelRebinding: (channelId: string) =>
    command<ChannelBinding>("begin_channel_rebinding", { channelId }),
  detectChannelGroups: (channelId: string) =>
    command<ChannelTarget[]>("detect_notification_channel_groups", { channelId }),
  detectBindingGroups: (bindingId: string) =>
    command<ChannelTarget[]>("detect_binding_groups", { bindingId }),
  setSystemNotificationsEnabled: (enabled: boolean) =>
    command<OperationResult>("set_system_notifications_enabled", { enabled }),
  saveMonitoringConfig: (config: MonitoringConfig) =>
    isPreview()
      ? Promise.resolve(previewSaveConfig(config))
      : command<OperationResult>("save_monitoring_config", { config }),
  setAutoStartMonitoring: (enabled: boolean) => command<OperationResult>("set_auto_start_monitoring", { enabled }),
  setNotificationUseSystemProxy: (enabled: boolean) => command<OperationResult>("set_notification_use_system_proxy", { enabled }),
  monitoringAction: (action: "start" | "pause" | "resume" | "restart") =>
    isPreview()
      ? Promise.resolve(previewMonitoringAction(action))
      : command<OperationResult>("monitoring_action", { action }),

  addProduct: (productId: string) =>
    isPreview()
      ? Promise.resolve(previewAddProduct(productId))
      : command<ProductRecord>("add_product", { productId }),
  setProductEnabled: (productId: string, enabled: boolean) =>
    isPreview()
      ? Promise.resolve(previewSetProductEnabled(productId, enabled))
      : command<OperationResult>("set_product_enabled", { productId, enabled }),
  setProductProminentAlert: (productId: string, enabled: boolean) =>
    command<OperationResult>("set_product_prominent_alert", { productId, enabled }),
  setOnboardingProducts: (productIds: string[]) =>
    command<OperationResult>("set_onboarding_products", { productIds }),
  testProminentAlert: (productId: string) =>
    command<OperationResult>("test_prominent_alert", { productId }),
  subscribeProminentAlert: async (receive: (alert: ProminentAlert & { presentationId: number; waitForFrame: boolean }) => void): Promise<() => void> => {
    if (isPreview()) {
      const alert = await command<ProminentAlert | null>("get_prominent_alert");
      if (alert) receive({ ...alert, presentationId: 0, waitForFrame: false });
      return () => {};
    }
    const channel = new Channel<ProminentAlert & { presentationId: number; waitForFrame: boolean }>();
    channel.onmessage = receive;
    await invoke("subscribe_prominent_alert", { channel });
    return () => { channel.onmessage = () => {}; };
  },
  showProminentAlert: (eventId: number, presentationId: number) => isPreview()
    ? Promise.resolve()
    : invoke<void>("show_prominent_alert", { eventId, presentationId }),
  confirmProminentAlertFrame: (eventId: number, presentationId: number) => isPreview()
    ? Promise.resolve()
    : invoke<void>("confirm_prominent_alert_frame", { eventId, presentationId }),
  dismissProminentAlert: (eventId: number) =>
    command<OperationResult>("dismiss_prominent_alert", { eventId }),
  removeProduct: (productId: string) =>
    command<OperationResult>("remove_product", { productId }),
  startProductScan: (startId: string, endId: string) =>
    command<ProductScan>("start_product_scan", { startId, endId }),
  controlProductScan: (action: "pause" | "resume" | "cancel") =>
    command<ProductScan>("control_product_scan", { action }),

  saveChannel: (channel: {
    id: string | null;
    name: string;
    providerId: string;
    bindingId?: string;
    targets?: ChannelTarget[];
    values: Record<string, string>;
    /** Secret只在保存命令中传递；空值沿用已保存内容，不会从快照读回。 */
    subscriptions: ChannelEvent[];
  }) => command<NotificationChannel>("save_notification_channel", { channel }),
  setChannelEnabled: (channelId: string, enabled: boolean) =>
    command<OperationResult>("set_notification_channel_enabled", { channelId, enabled }),
  removeChannel: (channelId: string) =>
    command<OperationResult>("remove_notification_channel", { channelId }),

  addProxy: (proxy: {
    protocol: ProxyRecord["protocol"];
    host: string;
    port: number;
    username: string;
    password: string;
  }) => command<ProxyRecord>("add_proxy", { proxy }),
  importProxies: (entries: string) =>
    command<ProxyImportResult>("import_proxies", { entries }),
  testProxy: (proxyId: string) => command<ProxyRecord>("test_proxy", { proxyId }),
  setProxyEnabled: (proxyId: string, enabled: boolean) =>
    command<OperationResult>("set_proxy_enabled", { proxyId, enabled }),
  removeProxy: (proxyId: string) => command<OperationResult>("remove_proxy", { proxyId }),

  setLoginStart: (enabled: boolean) =>
    command<PlatformSnapshot>("set_login_start", { enabled }),
  openLogsDirectory: () => command<OperationResult>("open_logs_directory"),
  openExternalUrl: (url: string) => command<OperationResult>("open_external_url", { url }),
  exportConfigurationFile: () => command<string | null>("export_configuration_file"),
  importConfigurationFile: () => command<string | null>("import_configuration_file"),
  restoreDefaults: (clearHistory: boolean) =>
    command<OperationResult>("restore_defaults", { clearHistory }),
  completeSetup: () => command<OperationResult>("complete_setup"),
  refreshNotificationPermission: () => command<PlatformSnapshot>("refresh_notification_permission"),
  requestNotificationPermission: () => command<PlatformSnapshot>("request_notification_permission"),
  createDiagnosticPreview: () => command<string>("create_diagnostic_preview"),
  saveDiagnosticReport: (contents: string) =>
    command<{ path: string }>("save_diagnostic_report", { contents }),
};

export async function sendSystemTestNotification(): Promise<
  "accepted_by_system" | "permission_denied"
> {
  return command<"accepted_by_system" | "permission_denied">("test_system_notification");
}
