<script lang="ts">
  import { onMount, tick } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";
  import appInfo from "../../src-tauri/tauri.conf.json";
  import { get } from "svelte/store";
  import { createVirtualizer } from "@tanstack/svelte-virtual";
  import {
    desktopApi,
    formatMonitoringDuration,
    productImageSrc,
    sendSystemTestNotification,
    type ChannelEvent,
    type ChannelBinding as BindingSession,
    type ChannelTarget,
    type DesktopSnapshot,
    type MonitoringConfig,
    type NotificationChannel,
    type ProductRecord,
    type ProxyRecord,
    type ScheduleConfig,
    type Weekday,
    type UpdateStatus,
  } from "$lib/desktop-api";
  import UpdateNotice from "$lib/UpdateNotice.svelte";
  import ScheduleFields from "$lib/ScheduleFields.svelte";
  import RateFields from "$lib/RateFields.svelte";
  import ProductTile from "$lib/ProductTile.svelte";
  import Onboarding from "$lib/Onboarding.svelte";
  import ProminentIcon from "$lib/ProminentIcon.svelte";
  import ProductChoice from "$lib/ProductChoice.svelte";
  import ChannelAdd from "$lib/ChannelAdd.svelte";
  import ChannelBinding from "$lib/ChannelBinding.svelte";
  import ProviderIcon from "$lib/ProviderIcon.svelte";
  import ChannelStatus from "$lib/ChannelStatus.svelte";
  import RecipientChip from "$lib/RecipientChip.svelte";
  import "$lib/theme.css";
  import ActivityIcon from "@lucide/svelte/icons/activity";
  import Bell from "@lucide/svelte/icons/bell";
  import CalendarDays from "@lucide/svelte/icons/calendar-days";
  import ChevronRight from "@lucide/svelte/icons/chevron-right";
  import Info from "@lucide/svelte/icons/info";
  import ListFilter from "@lucide/svelte/icons/list-filter";
  import Monitor from "@lucide/svelte/icons/monitor";
  import Moon from "@lucide/svelte/icons/moon";
  import Package from "@lucide/svelte/icons/package";
  import PanelRightClose from "@lucide/svelte/icons/panel-right-close";
  import PanelRightOpen from "@lucide/svelte/icons/panel-right-open";
  import Pause from "@lucide/svelte/icons/pause";
  import Pencil from "@lucide/svelte/icons/pencil";
  import Play from "@lucide/svelte/icons/play";
  import Plus from "@lucide/svelte/icons/plus";
  import Search from "@lucide/svelte/icons/search";
  import Send from "@lucide/svelte/icons/send";
  import Settings2 from "@lucide/svelte/icons/settings-2";
  import ShieldAlert from "@lucide/svelte/icons/shield-alert";
  import SlidersHorizontal from "@lucide/svelte/icons/sliders-horizontal";
  import Sun from "@lucide/svelte/icons/sun";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import X from "@lucide/svelte/icons/x";

  type Page = "status" | "notifications" | "rate" | "proxies" | "products" | "settings" | "setup";
  type ThemePreference = "light" | "dark" | "system";
  let theme: ThemePreference = "system";
  type Notice = { kind: "error" | "success"; text: string };
  import type { MessageCursor, MessageItem } from "$lib/desktop-api";

  const pages = [
    { id: "status", label: "状态", icon: ActivityIcon },
    { id: "notifications", label: "通知", icon: Bell },
    { id: "rate", label: "速率", icon: SlidersHorizontal },
    { id: "proxies", label: "代理池", icon: ShieldAlert },
    { id: "products", label: "监控产品", icon: Package },
    { id: "settings", label: "其他设置", icon: Settings2 },
  ] as const;
  const events: { id: ChannelEvent; label: string }[] = [
    { id: "stock_available", label: "商品上架或补货" },
    { id: "monitoring_failed", label: "监控异常" },
    { id: "recovered", label: "监控恢复" },
  ];
  const days: { id: Weekday; label: string }[] = [
    { id: "mon", label: "一" }, { id: "tue", label: "二" }, { id: "wed", label: "三" },
    { id: "thu", label: "四" }, { id: "fri", label: "五" }, { id: "sat", label: "六" }, { id: "sun", label: "日" },
  ];
  const defaultSchedule = (): ScheduleConfig => ({
    days: days.map((day) => day.id), start: "09:00", end: "19:00",
  });
  const defaultConfig = (): MonitoringConfig => ({
    autoStartMonitoring: true,
    monitoringMode: "listed_products",
    schedule: defaultSchedule(),
    rate: {
      intervalMinMs: 1000, intervalMaxMs: 2000,
      failuresBeforeBackoff: 3, failureBackoffSeconds: 20,
    },
    useSystemProxy: false, useProxyPool: false, failureAlertAfterMinutes: 10,
  });
  const messageDayFormat = new Intl.DateTimeFormat("zh-CN", { month: "numeric", day: "numeric", timeZone: "Asia/Shanghai" });
  const messageClockFormat = new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23", timeZone: "Asia/Shanghai" });

  let page: Page = "status";
  let mainElement: HTMLElement;
  let snapshot: DesktopSnapshot | null = null;
  let snapshotRequest = 0;
  let firstSnapshot = true;
  let config = defaultConfig();
  let loading = true;
  let busy = false;
  let busyCount = 0;
  let pendingTests = new Set<string>();
  let pendingToggles = new Set<string>();
  let savingChannel = false;
  let error = "";
  let notice: Notice | null = null;
  let productId = "";
  let previewAlertOpen = false;
  let prominentTested = false;
  let prominentPromptProduct: ProductRecord | null = null;
  let prominentPromptDialog: HTMLDialogElement;
  let scanStart = "65";
  let scanEnd = "245";
  let channel: NotificationChannel | null = null;
  let channelFormOpen = false;
  let channelFormExpanded = false;
  let channelConnected = false;
  let channelRebindOnMount = false;
  let channelBindingEditor: ChannelBinding;
  let channelToDeleteId: string | null = null;
  let deleteConfirmation: HTMLDivElement;
  let proxyToRemoveId: string | null = null;
  let proxyRemoveConfirmation: HTMLDivElement;
  let channelDialog: HTMLDialogElement;
  let channelError = "";
  let channelName = "";
  let providerId = "";
  let channelValues: Record<string, string> = {};
  let channelEvents: ChannelEvent[] = ["stock_available"];
  let channelMode: "binding" | "manual" = "binding";
  let channelBinding: BindingSession | null = null;
  let channelTargetId = "";
  let channelTargetKind = "chat";
  let channelSelectedTargets: ChannelTarget[] = [];
  let proxy: ProxyRecord["protocol"] = "http";
  let proxyHost = "";
  let proxyPort = "";
  let proxyUsername = "";
  let proxyPassword = "";
  let proxyImport = "";
  let proxyDialog: HTMLDialogElement;
  let detailDialog: HTMLDialogElement;
  let detailTitle = "";
  let detailError = "";
  let proxyDialogOpen = false;
  let proxyTab: "single" | "batch" = "single";
  let diagnostic = "";
  let clearHistory = false;
  let resetOpen = false;
  let appVersion = appInfo.version;
  let updateStatus: UpdateStatus = { phase: "idle", version: null, notes: null, downloaded: 0, total: null, error: null };
  let updateEventRevision = 0;
  let updateChecked = false;
  let showDateFilter = false;
  let showProductFilter = false;
  let messageDate = "";
  let messageProduct = "";
  let visibleMessages: MessageItem[] = [];
  let messagePage = 0;
  let messageCursors: (MessageCursor | null)[] = [null];
  let messageNextCursor: MessageCursor | null = null;
  let messageLoading = false;
  let messageError = "";
  let messageRequest = 0;
  let messageHasUpdates = false;
  let knownMessageProducts: [string, string][] = [];
  const maxMessagePages = 200;
  let productSearch = "";
  let productFilter: "all" | "selected" | "unselected" | "prominent" = "all";
  let productLookupOpen = false;
  let scanResultDismissed = false;
  let selectedProductId: string | null = null;
  let productToRemoveId: string | null = null;
  let productDeleteConfirmation: HTMLDivElement;
  let detailMessages: MessageItem[] = [];
  let detailLoading = false;
  let detailMessageError = "";
  let detailRequest = 0;
  let detailCloseButton: HTMLButtonElement;
  let detailOrigin: HTMLElement | null = null;
  let statusWorkspace: HTMLDivElement;
  let workspaceWidth = 1000;
  let messagesCollapsed = false;
  let messagesWidth = 320;
  let resizingMessages = false;
  let resizeStartWidth = 320;
  $: messageWidthLimit = Math.max(220, Math.round((workspaceWidth || 1000) * .6));
  $: effectiveMessagesWidth = Math.min(messagesWidth, messageWidthLimit);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let messageScroll: HTMLDivElement;
  const messageVirtualizer = createVirtualizer<HTMLDivElement, HTMLTableRowElement>({
    count: 0,
    getScrollElement: () => messageScroll,
    estimateSize: () => 48,
    overscan: 4,
    initialRect: { width: 460, height: 360 },
  });

  $: currentProvider = snapshot?.providers.find((item) => item.id === providerId) ?? null;
  $: channelFormExpanded = channelFormOpen && (channelMode === "manual" || !currentProvider?.supportsBinding || channelConnected && providerId !== "weixin");
  $: channelFirstMessagePending = channelMode === "binding" && channelBinding?.status === "complete" && (providerId === "weixin" ? channelBinding.privateMessageReceived !== true : channelSelectedTargets.some(target => target.kind === "user") && channelBinding.privateChatReady === false);
  $: defaultChannelName = channelBinding?.botName || editingChannel?.botName || channelDefaultName(providerId);
  $: if (snapshot?.scan?.status === "running") scanResultDismissed = false;
  $: editingChannel = channel ? snapshot?.channels.find((item) => item.id === channel?.id) ?? channel : null;
  $: activeProducts = snapshot?.products.filter((item) => item.enabled) ?? [];
  $: activeChannels = snapshot?.channels.filter((item) => item.enabled) ?? [];
  $: globalError = snapshot?.runtime?.lastError && (snapshot.runtime.state === "worker_failed" || !activeProducts.some((product) => product.runtimeError === snapshot?.runtime?.lastError))
    ? snapshot.runtime.lastError : null;
  $: productRows = (() => {
    const configured = snapshot?.products ?? [];
    const catalogOnly = new Map((snapshot?.catalog ?? []).map((item) => [item.productId, item]));
    for (const item of configured) catalogOnly.delete(item.productId);
    return [...configured, ...catalogOnly.values()].filter((item) => !/test/i.test(item.name)).sort((a, b) => Number(a.productId) - Number(b.productId));
  })();
  $: visibleProducts = productRows.filter((item) => {
    const selected = snapshot?.products.some((product) => product.productId === item.productId && product.enabled) ?? false;
    return (productFilter === "all" || (productFilter === "prominent" ? selected && item.prominentAlert : (productFilter === "selected") === selected))
      && (!productSearch || `${item.name} ${item.productId}`.toLocaleLowerCase().includes(productSearch.toLocaleLowerCase().trim()));
  });
  $: selectedProduct = selectedProductId ? productRows.find((item) => item.productId === selectedProductId) ?? null : null;
  $: {
    clearTimeout(toastTimer);
    if (notice?.kind === "success") toastTimer = setTimeout(() => notice = null, 3500);
  }
  $: messageProducts = [...new Map([...knownMessageProducts, ...[...(snapshot?.catalog ?? []), ...(snapshot?.products ?? [])].map((item) => [item.productId, item.name] as const)]).entries()];
  $: if (messageScroll) get(messageVirtualizer).setOptions({ count: visibleMessages.length, getScrollElement: () => messageScroll });
  $: savedConfig = snapshot?.config ?? defaultConfig();
  $: rateDirty = JSON.stringify(rateFields(config)) !== JSON.stringify(rateFields(savedConfig));
  $: networkDirty = config.useSystemProxy !== savedConfig.useSystemProxy || config.useProxyPool !== savedConfig.useProxyPool;
  $: modeDirty = config.monitoringMode !== savedConfig.monitoringMode;
  $: configDirty = rateDirty || networkDirty || modeDirty;

  function rateFields(value: MonitoringConfig) {
    return {
      schedule: { ...value.schedule, days: [...value.schedule.days].sort() },
      rate: value.rate,
      failureAlertAfterMinutes: value.failureAlertAfterMinutes,
    };
  }

  function setMessagesWidth(width: number) {
    messagesWidth = Math.round(Math.max(220, Math.min(width, messageWidthLimit)));
    localStorage.setItem("rm.messages.width", String(messagesWidth));
  }

  function toggleMessages() {
    messagesCollapsed = !messagesCollapsed;
    localStorage.setItem("rm.messages.collapsed", String(messagesCollapsed));
  }

  function startMessageResize(event: PointerEvent) {
    resizingMessages = true;
    resizeStartWidth = effectiveMessagesWidth;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function resizeMessages(event: PointerEvent) {
    if (resizingMessages) setMessagesWidth(statusWorkspace.getBoundingClientRect().right - event.clientX);
  }

  function onMessageResizeKey(event: KeyboardEvent) {
    if (!["ArrowLeft", "ArrowRight", "Home", "End", "Escape"].includes(event.key)) return;
    event.preventDefault();
    if (event.key === "Escape") resizingMessages = false;
    setMessagesWidth(event.key === "Escape" ? resizeStartWidth : event.key === "Home" ? 220 : event.key === "End" ? messageWidthLimit : effectiveMessagesWidth + (event.key === "ArrowLeft" ? 24 : -24));
  }

  function toggleMessageFilter(next: "date" | "product") {
    if (next === "date") {
      showDateFilter = !showDateFilter;
      if (!showDateFilter) messageDate = "";
    } else {
      showProductFilter = !showProductFilter;
      if (!showProductFilter) messageProduct = "";
    }
    void resetMessageQuery();
  }

  async function loadMessages() {
    const request = ++messageRequest;
    messageLoading = true;
    messageError = "";
    try {
      const result = await desktopApi.queryMessages({ date: messageDate || null, productId: messageProduct || null, cursor: messageCursors[messagePage], limit: 100 });
      if (request !== messageRequest) return;
      visibleMessages = result.items;
      knownMessageProducts = [...new Map([...knownMessageProducts, ...result.items.map((item) => [item.productId, item.name] as const)]).entries()].slice(-1000);
      messageNextCursor = messagePage + 1 < maxMessagePages ? result.nextCursor : null;
      messageHasUpdates = false;
      resetMessageScroll();
    } catch (cause) {
      if (request === messageRequest) messageError = message(cause);
    } finally {
      if (request === messageRequest) messageLoading = false;
    }
  }

  async function resetMessageQuery() {
    messagePage = 0;
    messageCursors = [null];
    messageNextCursor = null;
    await loadMessages();
  }

  async function changeMessagePage(next: number) {
    if (next < 0 || (next > messagePage && !messageNextCursor)) return;
    if (next > messagePage) messageCursors[next] = messageNextCursor;
    messagePage = next;
    await loadMessages();
  }

  async function onProxyTabKeydown(event: KeyboardEvent) {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    proxyTab = event.key === "Home" ? "single" : event.key === "End" ? "batch" : proxyTab === "single" ? "batch" : "single";
    await tick();
    proxyDialog?.querySelector<HTMLButtonElement>('[role="tab"][aria-selected="true"]')?.focus();
  }

  function resetMessageScroll() {
    if (messageScroll) messageScroll.scrollTop = 0;
  }

  function navigate(next: Page) {
    if (page === next) return;
    closeProductDetail(false);
    channelToDeleteId = null;
    proxyToRemoveId = null;
    page = next;
    if (next === "products" || next === "status" || next === "setup") {
      void desktopApi.refreshCatalog().then(refresh).catch((cause) => { notice = { kind: "error", text: message(cause) }; });
    }
    if (mainElement) mainElement.scrollTop = 0;
    if (window.innerWidth <= 700) window.scrollTo(0, 0);
  }

  function adopt(next: DesktopSnapshot) {
    snapshotRequest++;
    error = "";
    const changed = snapshot && (next.recentChecks[0]?.at !== snapshot.recentChecks[0]?.at || next.recentEvents[0]?.id !== snapshot.recentEvents[0]?.id);
    snapshot = next;
    if (firstSnapshot) void loadMessages();
    else if (changed) {
      if (page === "status" && messagePage === 0 && !messageDate && !messageProduct && (!messageScroll || messageScroll.scrollTop === 0)) void loadMessages();
      else messageHasUpdates = true;
    }
    if (!configDirty && next.config) config = structuredClone(next.config);
    if (firstSnapshot && !next.setupCompleted) navigate("setup");
    firstSnapshot = false;
  }

  async function refresh() {
    const request = ++snapshotRequest;
    try {
      const next = await desktopApi.getSnapshot();
      if (request !== snapshotRequest) return;
      adopt(next);
      error = "";
    } catch (cause) {
      if (request === snapshotRequest) error = message(cause);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    prominentTested = localStorage.getItem("rm.prominent.tested") === "true";
    const savedTheme = localStorage.getItem("rm.theme");
    theme = savedTheme === "light" || savedTheme === "dark" ? savedTheme : "system";
    document.documentElement.dataset.theme = theme;
    let stop: (() => void) | undefined;
    let stopErrors: (() => void) | undefined;
    let stopUpdates: (() => void) | undefined;
    let alive = true;
    if (!desktopApi.previewMode) void getVersion().then((version) => { if (alive) appVersion = version; }).catch(() => {});
    void desktopApi.subscribe((next) => alive && adopt(next)).then((unlisten) => {
      stop = unlisten;
      if (!alive) stop();
    }).catch((cause) => { error = message(cause); });
    void desktopApi.subscribeErrors((cause) => {
      if (alive) notice = { kind: "error", text: cause };
    }).then((unlisten) => {
      stopErrors = unlisten;
      if (!alive) stopErrors();
    }).catch((cause) => { error = message(cause); });
    void desktopApi.subscribeUpdates((status) => {
      if (alive) {
        updateEventRevision++;
        updateStatus = status;
      }
    }).then((unlisten) => {
      stopUpdates = unlisten;
      if (!alive) stopUpdates();
      else if (!desktopApi.previewMode) void checkUpdates();
    }).catch((cause) => {
      if (alive) {
        updateStatus = { ...updateStatus, error: message(cause) };
        void checkUpdates();
      }
    });
    void refresh().then(() => desktopApi.refreshCatalog()).catch((cause) => notice = { kind: "error", text: message(cause) });
    window.addEventListener("focus", refresh);
    scanStart = localStorage.getItem("ricoh.scan.start") ?? scanStart;
    scanEnd = localStorage.getItem("ricoh.scan.end") ?? scanEnd;
    const savedWidth = Number(localStorage.getItem("rm.messages.width"));
    if (Number.isFinite(savedWidth) && savedWidth >= 220) messagesWidth = Math.min(savedWidth, 900);
    messagesCollapsed = localStorage.getItem("rm.messages.collapsed") === "true";
    const dismissPreviewAlert = () => previewAlertOpen = false;
    window.addEventListener("rm-preview-alert-close", dismissPreviewAlert);
    return () => { alive = false; snapshotRequest++; stop?.(); stopErrors?.(); stopUpdates?.(); window.removeEventListener("focus", refresh); window.removeEventListener("rm-preview-alert-close", dismissPreviewAlert); clearTimeout(toastTimer); };
  });

  function changeTheme(preference: ThemePreference) {
    theme = preference;
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("rm.theme", theme);
  }

  function message(cause: unknown) {
    return cause instanceof Error ? cause.message : "操作没有完成，请重试。";
  }

  function beginOperation() {
    busyCount += 1;
    busy = true;
  }

  function endOperation() {
    busyCount -= 1;
    busy = busyCount > 0;
  }

  async function run(action: () => Promise<unknown>, success?: string) {
    beginOperation();
    notice = null;
    try {
      await action();
      if (success) notice = { kind: "success", text: success };
      await refresh();
      return true;
    } catch (cause) {
      await refresh();
      notice = { kind: "error", text: message(cause) };
      return false;
    } finally {
      endOperation();
    }
  }

  async function saveToggle(key: string, input: HTMLInputElement, current: boolean, action: (enabled: boolean) => Promise<unknown>) {
    if (pendingToggles.has(key)) { input.checked = current; return; }
    pendingToggles = new Set(pendingToggles).add(key);
    const requested = input.checked;
    try {
      if (!await run(() => action(requested))) input.checked = current;
    } finally {
      pendingToggles = new Set([...pendingToggles].filter((item) => item !== key));
    }
  }

  async function toggleProduct(id: string, enabled: boolean) {
    const key = `product:${id}`;
    if (pendingToggles.has(key)) return;
    pendingToggles = new Set(pendingToggles).add(key);
    try {
      await run(() => desktopApi.setProductEnabled(id, enabled));
    } finally {
      pendingToggles = new Set([...pendingToggles].filter((item) => item !== key));
    }
  }

  async function toggleProminent(id: string, enabled: boolean) {
    if (enabled && !prominentTested) {
      prominentPromptProduct = productRows.find((product) => product.productId === id) ?? null;
      await tick();
      prominentPromptDialog?.showModal();
      return;
    }
    const key = `prominent:${id}`;
    if (pendingToggles.has(key)) return;
    pendingToggles = new Set(pendingToggles).add(key);
    try { await run(() => desktopApi.setProductProminentAlert(id, enabled)); }
    finally { pendingToggles = new Set([...pendingToggles].filter((item) => item !== key)); }
  }

  function editConfig(patch: Partial<MonitoringConfig>) {
    config = { ...config, ...patch };
  }

  function resetConfig(scope: "rate" | "network" | "all" = "all") {
    const saved = structuredClone(snapshot?.config ?? defaultConfig());
    if (scope === "all") config = saved;
    else if (scope === "rate") config = { ...config, ...rateFields(saved) };
    else config = { ...config, useSystemProxy: saved.useSystemProxy, useProxyPool: saved.useProxyPool };
  }

  function editSchedule(patch: Partial<ScheduleConfig>) {
    editConfig({ schedule: { ...config.schedule, ...patch } });
  }

  function editRate(key: keyof MonitoringConfig["rate"], value: number) {
    editConfig({ rate: { ...config.rate, [key]: value } });
  }

  function toggleDay(day: Weekday) {
    const next = config.schedule.days.includes(day)
      ? config.schedule.days.filter((item) => item !== day)
      : [...config.schedule.days, day];
    editSchedule({ days: next });
  }

  async function saveConfig(scope: "rate" | "network" | "mode") {
    const next = scope === "rate"
      ? { ...savedConfig, ...rateFields(config) }
      : scope === "mode" ? { ...savedConfig, monitoringMode: config.monitoringMode }
      : { ...savedConfig, useSystemProxy: config.useSystemProxy, useProxyPool: config.useProxyPool };
    await run(() => desktopApi.saveMonitoringConfig(next), "设置已保存。");
  }

  function time(value: string | null | undefined) {
    if (!value) return "—";
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;
    return new Intl.DateTimeFormat("zh-CN", {
      month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit",
      hourCycle: "h23", timeZone: "Asia/Shanghai",
    }).format(date);
  }

  function timePrecise(value: string | null | undefined) {
    if (!value) return "—";
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;
    return new Intl.DateTimeFormat("zh-CN", {
      month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit",
      fractionalSecondDigits: 3, hourCycle: "h23", timeZone: "Asia/Shanghai",
    }).format(date);
  }

  function messageDay(value: string) {
    return messageDayFormat.format(new Date(value));
  }

  function messageClock(value: string) {
    return messageClockFormat.format(new Date(value));
  }

  function monitoringDuration(milliseconds: number | undefined) {
    return formatMonitoringDuration(milliseconds ?? 0);
  }

  function productState(product: ProductRecord) {
    const stock = product.observation?.stock;
    return product.runtimeError || !product.observation || stock === null ? showLabel(product) : `${showLabel(product)} · ${availability(product)} ${stock}`;
  }

  async function openProductDetail(product: ProductRecord, origin: EventTarget | null) {
    detailOrigin = origin instanceof HTMLElement ? origin : null;
    selectedProductId = product.productId;
    productToRemoveId = null;
    detailMessages = [];
    detailMessageError = "";
    detailLoading = true;
    const request = ++detailRequest;
    await tick();
    detailCloseButton?.focus();
    try {
      const result = await desktopApi.queryMessages({ date: null, productId: product.productId, cursor: null, limit: 3 });
      if (request === detailRequest) detailMessages = result.items;
    } catch (cause) {
      if (request === detailRequest) detailMessageError = message(cause);
    } finally {
      if (request === detailRequest) detailLoading = false;
    }
  }

  function closeProductDetail(restoreFocus = true) {
    if (!selectedProductId) return;
    selectedProductId = null;
    productToRemoveId = null;
    detailRequest += 1;
    if (restoreFocus) detailOrigin?.focus();
  }

  function handleEscape(event: KeyboardEvent) {
    if (event.key === "Escape" && !document.querySelector("dialog[open]") && selectedProductId) closeProductDetail();
  }

  async function requestProductRemoval(product: ProductRecord) {
    if (busy) return;
    if (!product.enabled) { await removeProduct(product.productId); return; }
    productToRemoveId = product.productId;
    await tick();
    productDeleteConfirmation?.focus();
  }

  async function removeProduct(id: string) {
    if (busy) return;
    if (await run(() => desktopApi.removeProduct(id), "商品已删除。") && selectedProductId === id) {
      closeProductDetail(false);
      await tick();
      mainElement?.querySelector<HTMLInputElement>(".product-search input")?.focus();
    }
  }

  function availability(product: ProductRecord) {
    const state = product.observation?.availability;
    if (product.runtimeError) return "检查失败";
    if (state === "in_stock") return "有货";
    if (state === "out_of_stock") return "无货";
    return "等待检查";
  }

  function showError(title: string, error: string) {
    detailTitle = title;
    detailError = error;
    detailDialog.showModal();
  }

  function showLabel(product: ProductRecord) {
    if (product.runtimeError) return "检查失败";
    if (!product.observation) return "尚无成功检查";
    const value = product.observation?.isShow;
    return value === 1 ? "已上架" : value === 0 ? "未上架" : "尚无成功检查";
  }

  function channelDefaultName(id: string) {
    const provider = snapshot?.providers.find((item) => item.id === id);
    const base = provider?.name ?? "通知";
    const count = snapshot?.channels.filter((item) => item.providerId === id).length ?? 0;
    return `${base} ${count + 1}`;
  }

  function deliveryEvent(event: ChannelEvent | "test") {
    return event === "test" ? "测试通知" : events.find((item) => item.id === event)?.label ?? "通知";
  }

  function deliveryOutcome(outcome: "accepted" | "failed" | "unknown") {
    if (outcome === "accepted") return "已发送";
    if (outcome === "failed") return "投递失败";
    return "尚未确认送达";
  }

  async function addProduct(id = productId.trim()) {
    if (busy || !id) return false;
    const normalizedId = id.replace(/^0+(?=\d)/, "");
    const exists = productRows.some((product) => product.productId === normalizedId);
    if (exists && (page !== "setup" || activeProducts.some((product) => product.productId === normalizedId))) {
      notice = { kind: "success", text: "商品已在列表中。" };
      return false;
    }
    const validation = await run(async () => {
      if (exists) await desktopApi.setProductEnabled(normalizedId, true);
      else await desktopApi.addProduct(normalizedId);
      productId = "";
    }, "商品已添加。");
    return validation;
  }

  async function editChannel(existing: NotificationChannel | null = null, selectedProvider?: string) {
    channelFormOpen = true;
    channelError = "";
    notice = null;
    channel = existing;
    channelConnected = false;
    channelFormExpanded = false;
    channelRebindOnMount = false;
    channelName = existing?.name ?? "";
    providerId = existing?.providerId ?? selectedProvider ?? snapshot?.providers[0]?.id ?? "";
    channelValues = {};
    channelBinding = null;
    channelMode = "binding";
    channelTargetId = existing?.targetId ?? "";
    channelTargetKind = existing?.targetKind ?? "chat";
    channelSelectedTargets = (existing?.selectedTargets ?? []).map((target) => ({ ...target }));
    channelEvents = existing?.subscriptions ?? ["stock_available"];
    await tick();
    channelDialog?.showModal();
    channelDialog?.querySelector<HTMLButtonElement>("button[aria-label='关闭']")?.focus();
  }

  async function testProminentAlert() {
    const product = activeProducts[0] ?? productRows[0];
    if (!product) return;
    await showProminentTest(product.productId);
  }

  async function showProminentTest(id: string) {
    const shown = await run(async () => {
      await desktopApi.testProminentAlert(id);
      prominentTested = true;
      localStorage.setItem("rm.prominent.tested", "true");
    });
    if (shown && desktopApi.previewMode) previewAlertOpen = true;
    return shown;
  }

  function closeProminentPrompt() {
    prominentPromptDialog?.close();
    prominentPromptProduct = null;
  }

  async function confirmProminentTest() {
    if (!prominentPromptProduct || busy) return;
    const id = prominentPromptProduct.productId;
    if (await showProminentTest(id)) {
      closeProminentPrompt();
      await toggleProminent(id, true);
    }
  }

  function closeChannel() {
    if (channelDialog?.open) channelDialog.close();
    channelFormOpen = false;
    channelError = "";
    channel = null;
    channelValues = {};
    channelBinding = null;
    channelSelectedTargets = [];
  }

  function connectionLabel(status?: string) {
    return status === "ready" ? "已连接" : status === "auth_required" ? "需要重新授权" : status === "connection_conflict" ? "连接被其他实例接管" : status === "connecting" ? "连接中" : status === "reconnecting" ? "重新连接中" : status === "failed" ? "连接失败" : status === "stopped" ? "连接已停止" : status === "awaiting_message" ? "等待首条消息" : "";
  }

  async function saveChannel() {
    if (!providerId || savingChannel) return;
    if (channelFirstMessagePending) { channelError = providerId === "feishu" ? "请打开机器人应用，再返回保存。" : "请向机器人发送任意私信，再返回保存。"; return; }
    const enableAfterTest = !channel || channel.enabled;
    const usingBinding = currentProvider?.supportsBinding && channelMode === "binding";
    const manualTargetId = channelValues.targetId?.trim();
    const targets = usingBinding || !manualTargetId ? channelSelectedTargets : [{ id: manualTargetId, kind: channelValues.targetKind?.trim() || "chat", label: manualTargetId }];
    const name = channelName.trim() || channel?.name || defaultChannelName;
    const sameTargets = (a: ChannelTarget[], b: ChannelTarget[]) => a.length === b.length && a.every((target) => b.some((other) => target.id === other.id && target.kind === other.kind && target.label === other.label));
    const credentialsChanged = !channel || !!channelBinding || Object.entries(channelValues).some(([key, value]) => !key.startsWith("target") && value.trim());
    const recipientsChanged = !channel || !sameTargets(targets, channel.selectedTargets ?? []);
    const subscriptionsChanged = !channel || channelEvents.length !== channel.subscriptions.length || channelEvents.some((event) => !channel!.subscriptions.includes(event));
    if (channel && !credentialsChanged && !recipientsChanged && !subscriptionsChanged && name === channel.name) { closeChannel(); return; }
    if (usingBinding && ((!channel && channelBinding?.status !== "complete") || channelBinding && channelBinding.status !== "complete" || channel?.connectionStatus === "auth_required" && channelBinding?.status !== "complete")) {
      channelError = "请先扫码完成连接。";
      return;
    }
    if (usingBinding && !channelSelectedTargets.length) {
      channelError = "请勾选至少一个通知接收对象，或手动添加接收对象。";
      return;
    }
    const requiredField = !usingBinding && currentProvider?.fields.find((field) => field.required && !channelValues[field.key]?.trim() && !channel?.configuredFieldKeys.includes(field.key) && !(targets.length && (field.key === "targetId" || field.key === "targetKind")));
    if (requiredField) {
      channelError = `请填写${requiredField.label}。`;
      return;
    }
    savingChannel = true;
    beginOperation();
    channelError = "";
    try {
      const saved = await desktopApi.saveChannel({
        id: channel?.id ?? null,
        name,
        providerId,
        ...(usingBinding && channelBinding?.status === "complete" ? { bindingId: channelBinding.id } : {}),
        ...(usingBinding || targets.length ? { targets: targets.map((target) => ({ ...target })) } : {}),
        values: usingBinding ? {} : {
          ...Object.fromEntries(Object.entries(channelValues).filter(([, value]) => value.trim())),
        },
        subscriptions: channelEvents,
      });
      closeChannel();
      await refresh();
      if (credentialsChanged) await testChannel(saved.id, enableAfterTest && !saved.enabled);
    } catch (cause) {
      if (channelFormOpen) channelError = message(cause);
      else notice = { kind: "error", text: message(cause) };
    } finally {
      savingChannel = false;
      endOperation();
    }
  }

  function toggleEvent(event: ChannelEvent) {
    channelEvents = channelEvents.includes(event)
      ? channelEvents.filter((item) => item !== event)
      : [...channelEvents, event];
  }

  async function testChannel(id: string, enableAfterTest = false) {
    if (pendingTests.has(id)) return;
    pendingTests = new Set(pendingTests).add(id);
    beginOperation();
    notice = null;
    try {
      const result = await desktopApi.testChannel(id);
      if (result.outcome === "accepted" && enableAfterTest) await desktopApi.setChannelEnabled(id, true);
      notice = result.outcome === "accepted"
        ? { kind: "success", text: result.message ? `测试成功：${result.message}` : "测试成功：平台已接受测试通知。" }
        : { kind: "error", text: result.message ?? (result.outcome === "unknown" ? "尚未确认测试是否送达。" : "测试未通过。") };
      await refresh();
    } catch (cause) {
      notice = { kind: "error", text: message(cause) };
    } finally {
      pendingTests = new Set([...pendingTests].filter((item) => item !== id));
      endOperation();
    }
  }

  async function askRemoveChannel(id: string) {
    channelToDeleteId = id;
    await tick();
    deleteConfirmation?.focus();
  }

  async function removeChannel(id: string) {
    channelToDeleteId = null;
    await run(() => desktopApi.removeChannel(id));
  }

  async function askRemoveProxy(id: string) {
    proxyToRemoveId = id;
    await tick();
    proxyRemoveConfirmation?.focus();
  }

  async function removeProxy(id: string) {
    proxyToRemoveId = null;
    await run(() => desktopApi.removeProxy(id));
  }

  async function validateAndScan() {
    if (!/^\d+$/.test(scanStart) || !/^\d+$/.test(scanEnd)) {
      notice = { kind: "error", text: "请输入有效的起始和结束 Product ID。" };
      return;
    }
    const start = Number(scanStart);
    const end = Number(scanEnd);
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 1 || end < start || end - start >= 1000) {
      notice = { kind: "error", text: "扫描范围最多包含 1000 个连续编号。" };
      return;
    }
    localStorage.setItem("ricoh.scan.start", scanStart);
    localStorage.setItem("ricoh.scan.end", scanEnd);
    scanResultDismissed = false;
    await run(() => desktopApi.startProductScan(scanStart, scanEnd), "已开始扫描。");
  }

  async function scanAction(action: "pause" | "resume" | "cancel") {
    if (snapshot?.scan) await run(() => desktopApi.controlProductScan(action));
  }

  async function addProxy() {
    const port = Number(proxyPort);
    if (!proxyHost.trim() || !Number.isInteger(port) || port < 1 || port > 65535) {
      notice = { kind: "error", text: "请填写有效的代理主机和端口。" };
      return;
    }
    const result = await run(() => desktopApi.addProxy({
      protocol: proxy, host: proxyHost.trim(), port, username: proxyUsername, password: proxyPassword,
    }), "代理已添加。");
    if (result) {
      [proxyHost, proxyPort, proxyUsername, proxyPassword] = ["", "", "", ""];
      closeProxyDialog();
    }
  }

  async function openProxyDialog() {
    proxyDialogOpen = true;
    proxyTab = "single";
    await tick();
    proxyDialog?.showModal();
  }

  function closeProxyDialog() {
    if (proxyDialog?.open) proxyDialog.close();
    proxyDialogOpen = false;
  }

  async function importProxies() {
    if (await run(() => desktopApi.importProxies(proxyImport), "导入完成。")) {
      proxyImport = "";
      closeProxyDialog();
    }
  }

  async function sendSystemTest() {
    await run(async () => {
      const result = await sendSystemTestNotification();
      if (result !== "accepted_by_system") throw new Error("系统通知权限未允许，可在系统设置中开启后重试。");
    }, desktopApi.previewMode ? "网页演示：未发送真实系统通知。" : "系统已接受测试通知。");
  }

  async function checkNotificationPermission(request = false) {
    if (request) {
      await run(() => desktopApi.requestNotificationPermission());
      return;
    }
    beginOperation();
    notice = null;
    try {
      const platform = await desktopApi.refreshNotificationPermission();
      if (snapshot) adopt({ ...snapshot, platform });
    } catch {
      // 检查结果以最终读取的权限状态为准，持续错误由平台状态区域展示。
      await refresh();
    } finally {
      endOperation();
    }
  }

  async function enableSetupNotifications() {
    await run(async () => {
      let platform = await desktopApi.refreshNotificationPermission();
      if (platform.notificationPermission !== "granted") platform = await desktopApi.requestNotificationPermission();
      if (platform.notificationPermission !== "granted") throw new Error("系统通知尚未允许，请在系统设置中开启。也可以跳过，稍后设置。");
      await desktopApi.setSystemNotificationsEnabled(true);
    }, "系统通知已启用。");
  }

  async function finishSetup(loginStart: boolean, autoStartMonitoring: boolean) {
    if (!activeProducts.length) {
      notice = { kind: "error", text: "请先添加至少一个监控商品。" };
      return;
    }
    const firstSetup = !snapshot?.setupCompleted;
    const ready = await run(async () => {
      if (firstSetup) await desktopApi.saveMonitoringConfig({ ...config, autoStartMonitoring, schedule: { days: days.map((day) => day.id), start: "00:00", end: "00:00" } });
      else if (autoStartMonitoring !== snapshot?.config?.autoStartMonitoring) await desktopApi.setAutoStartMonitoring(autoStartMonitoring);
      if (loginStart !== (snapshot?.platform?.loginStartEnabled ?? false)) await desktopApi.setLoginStart(loginStart);
      if (firstSetup) await desktopApi.completeSetup();
    }, desktopApi.previewMode ? "引导已完成，当前为网页演示。" : firstSetup && autoStartMonitoring ? "监控已开始。" : "设置已完成。");
    if (ready) navigate("status");
  }

  async function restoreDefaults() {
    if (await run(() => desktopApi.restoreDefaults(clearHistory), "已恢复默认设置。")) {
      resetConfig();
      resetOpen = false;
      navigate("setup");
    }
  }

  async function importConfigFile() {
    let imported = false;
    if (await run(async () => { imported = !!await desktopApi.importConfigurationFile(); }) && imported) {
      resetConfig();
      notice = { kind: "success", text: "配置已导入。" };
    }
  }

  async function checkUpdates() {
    if (desktopApi.previewMode) return;
    if (["checking", "downloading", "installing"].includes(updateStatus.phase)) return;
    updateStatus = { ...updateStatus, phase: "checking", error: null };
    const revision = updateEventRevision;
    try {
      const status = await desktopApi.checkForUpdates();
      updateChecked = !status.error;
      if (revision === updateEventRevision) updateStatus = status;
    } catch (cause) {
      updateChecked = false;
      if (revision === updateEventRevision) updateStatus = { ...updateStatus, phase: "idle", error: message(cause) };
    }
  }

  async function installUpdate() {
    if (updateStatus.phase !== "available") return;
    const revision = updateEventRevision;
    const previous = updateStatus;
    updateStatus = { ...updateStatus, phase: "installing", error: null };
    try {
      await desktopApi.installUpdate();
    } catch (cause) {
      if (revision === updateEventRevision) updateStatus = { ...previous, phase: "available", error: message(cause) };
    }
  }

  async function openAppLink(key: "projectUrl" | "tutorialUrl" | "feedbackUrl") {
    const url = snapshot?.platform?.[key];
    if (url) await run(() => desktopApi.openExternalUrl(url));
  }

  async function openProviderDocumentation() {
    if (currentProvider?.documentationUrl) await run(() => desktopApi.openExternalUrl(currentProvider.documentationUrl!));
  }

  function runtimeLabel(state: string | undefined) {
    return ({
      monitoring: "正在监控", partial_error: "部分检查失败", outside_schedule: "等待计划开始",
      paused: "已暂停", stopped: "已停止", setup_incomplete: "尚未设置", worker_failed: "监控任务已停止",
    } as Record<string, string>)[state ?? ""] ?? "正在读取";
  }
</script>

<svelte:head>
  <title>{appInfo.productName}</title>
</svelte:head>

<svelte:document on:keydown={handleEscape} />
<div class="shell" class:onboarding-shell={page === "setup"}>
  {#if page !== "setup"}
  <aside class="sidebar">
    <div class="brand-signature"><img class="brand-wordmark" src="/brand/rm-wordmark.svg" alt="RM" /><span class="brand-name">{appInfo.productName}</span></div>
    {#if desktopApi.previewMode}<p class="preview-label">界面预览 · 不连接监控</p>{/if}
    <nav aria-label="主导航">
      {#each pages as item}
        <button class:active={page === item.id} aria-current={page === item.id ? "page" : undefined} on:click={() => { navigate(item.id); notice = null; }}>
          <svelte:component this={item.icon} size={17} strokeWidth={1.5} aria-hidden="true" /><span>{item.label}</span>
        </button>
      {/each}
    </nav>
    <div class="sidebar-state">
      {#if snapshot?.runtime}<span><i class:running={snapshot.runtime.state === "monitoring"}></i>{snapshot.runtime.state === "outside_schedule" ? "等待" : runtimeLabel(snapshot.runtime.state)}</span>{/if}
      <a class="icon-button github-link" href="https://github.com/lavapapa/open-richo-monitor" target="_blank" rel="noreferrer" aria-label="项目主页" title="项目主页" on:click={(event) => { if (!desktopApi.previewMode) { event.preventDefault(); void run(() => desktopApi.openExternalUrl("https://github.com/lavapapa/open-richo-monitor")); } }}><span aria-hidden="true"></span></a>
      <button class="icon-button theme-switch" aria-label={`外观：${theme === "system" ? "跟随系统" : theme === "light" ? "浅色" : "深色"}`} title={`外观：${theme === "system" ? "跟随系统；切换为浅色" : theme === "light" ? "浅色；切换为深色" : "深色；切换为跟随系统"}`} on:click={() => changeTheme(theme === "system" ? "light" : theme === "light" ? "dark" : "system")}>
        {#if theme === "system"}<Monitor size={15} />{:else if theme === "light"}<Sun size={15} />{:else}<Moon size={15} />{/if}
      </button>
    </div>
  </aside>
  {/if}

  <main class:status-page={page === "status"} bind:this={mainElement}>
    {#if page !== "settings"}<UpdateNotice mode="banner" status={updateStatus} checked={updateChecked} onCheck={checkUpdates} onInstall={installUpdate} />{/if}
    {#if snapshot?.systemNotificationsEnabled && snapshot.platform?.notificationPermission === "denied"}<p class="notice error" role="alert">系统通知权限已关闭。请在系统设置中开启本应用通知，返回后点击“检查权限”。{#if page !== "notifications"}<button on:click={() => navigate("notifications")}>通知设置</button>{/if}</p>{/if}
    {#if snapshot?.systemNotificationsEnabled && snapshot.platform?.notificationPermissionError && notice?.text !== snapshot.platform.notificationPermissionError}<p class="notice error" role="alert">{snapshot.platform.notificationPermissionError}{#if page !== "notifications"}<button on:click={() => navigate("notifications")}>通知设置</button>{/if}</p>{/if}
    {#if error}<p class="notice error" role="alert">{error} <button on:click={refresh}>重试</button></p>{/if}
    {#if notice}<p class:success={notice.kind === "success"} class:error={notice.kind === "error"} class="notice toast" role={notice.kind === "error" ? "alert" : "status"}>{notice.text}<button aria-label="关闭" on:click={() => notice = null}>×</button></p>{/if}
    {#if loading && !snapshot}<p class="loading-state">正在读取状态…</p>
    {:else if page === "setup"}
      {#if snapshot}<Onboarding {snapshot} {busy} onCancel={() => { notice = null; navigate("status"); }} onProducts={(ids) => run(() => desktopApi.setOnboardingProducts(ids))} onSystemEnable={enableSetupNotifications} onSystemTest={sendSystemTest} onAddChannel={(id) => editChannel(null, id)} onChannelEdit={(item) => editChannel(item)} onChannelTest={testChannel} onChannelToggle={(id, enabled) => { void run(() => desktopApi.setChannelEnabled(id, enabled)); }} onComplete={finishSetup} />{/if}
    {:else if page === "status"}
      <section class="content status-content">
        <section class="status-line">
          <div class="status-overview"><span class="status-indicator" class:healthy={snapshot?.runtime?.state === "monitoring"} class:warning={snapshot?.runtime?.state === "partial_error" || snapshot?.runtime?.state === "worker_failed"} aria-hidden="true"></span><div><div class="status-heading"><h2>{runtimeLabel(snapshot?.runtime?.state)}</h2>{#if snapshot?.runtime?.state === "outside_schedule"}<p>北京时间下次开始：{time(snapshot.runtime.nextStartAt)}</p>{/if}</div>{#if snapshot?.runtime?.state === "worker_failed"}<p>以下为历史检查结果。重新打开应用后可继续监控。</p>{/if}{#if globalError}<button class="product-error-link" on:click={() => showError("监控异常", globalError)}>监控异常</button>{/if}</div></div>
          <div class="status-aux"><button class="text-button" on:click={() => navigate("notifications")}>通知渠道（{snapshot?.channels.length ?? 0}）</button><button class="text-button" on:click={() => navigate("proxies")}>代理池（{snapshot?.proxies.length ?? 0}）</button></div>
          {#if snapshot?.runtime?.state === "monitoring" || snapshot?.runtime?.state === "partial_error" || snapshot?.runtime?.state === "outside_schedule"}<button class="primary" disabled={busy} on:click={() => run(() => desktopApi.monitoringAction("pause"))}><Pause size={15} aria-hidden="true" />暂停</button>
          {:else if snapshot?.runtime?.state === "worker_failed"}<button class="primary" disabled>重新打开应用</button>
          {:else}<button class="primary" disabled={busy} on:click={() => run(() => desktopApi.monitoringAction(snapshot?.runtime?.state === "paused" ? "resume" : "start"))}><Play size={15} aria-hidden="true" />{snapshot?.runtime?.state === "paused" ? "继续" : "开始监控"}</button>{/if}
        </section>
        <div class="status-workspace" class:messages-collapsed={messagesCollapsed} class:resizing={resizingMessages} style:--messages-width={`${messagesWidth}px`} bind:this={statusWorkspace} bind:clientWidth={workspaceWidth}><section class="group status-products" aria-label="监控商品">
          {#if activeProducts.length}
            <div class="product-image-grid">{#each activeProducts as product (product.productId)}<ProductTile {product} compact selected={selectedProductId === product.productId} prominentPending={pendingToggles.has(`prominent:${product.productId}`)} onProminent={(enabled) => toggleProminent(product.productId, enabled)} onOpen={(origin) => openProductDetail(product, origin)} onToggle={(enabled) => toggleProduct(product.productId, enabled)} onError={(raw) => showError("检查失败", raw)} />{/each}</div>
          {:else}<p class="empty">{snapshot?.products.length ? "没有已启用的监控商品。" : "还没有监控商品。"}<button class="text-button" on:click={() => navigate("products")}>{snapshot?.products.length ? "管理商品" : "添加商品"}</button></p>{/if}
        </section>
        {#if !messagesCollapsed}
          <!-- 可聚焦的 separator 是键盘可操作的分隔条；编译器将该角色视为静态分隔线。 -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
          <div class="message-resizer" role="separator" tabindex="0" aria-controls="recent-messages" aria-label="调整最近消息宽度" aria-orientation="vertical" aria-valuemin="220" aria-valuemax={messageWidthLimit} aria-valuenow={effectiveMessagesWidth} on:pointerdown={startMessageResize} on:pointermove={resizeMessages} on:pointerup={() => resizingMessages = false} on:lostpointercapture={() => resizingMessages = false} on:keydown={onMessageResizeKey}></div>
        {/if}
        <section class="group recent-messages" id="recent-messages" aria-label="最近消息">
          <div class="section-heading"><h2>{messagesCollapsed ? "消息" : "最近消息"}</h2><div class="message-tools">
            {#if !messagesCollapsed}<button class="filter-button" aria-label="按日期筛选" title="按日期筛选" aria-pressed={showDateFilter} on:click={() => toggleMessageFilter("date")}><CalendarDays size={14} aria-hidden="true" /></button>
            <button class="filter-button" aria-label="按商品筛选" title="按商品筛选" aria-pressed={showProductFilter} on:click={() => toggleMessageFilter("product")}><ListFilter size={14} aria-hidden="true" /></button>
            {#if messageDate || messageProduct}<button class="text-button" on:click={() => { messageDate = ""; messageProduct = ""; void resetMessageQuery(); }}>清除</button>{/if}
            <button class="text-button" disabled={messageLoading} on:click={() => void loadMessages()}>刷新</button>
            {/if}<button class="icon-button collapse-messages" aria-label={messagesCollapsed ? "展开最近消息" : "折叠最近消息"} title={messagesCollapsed ? "展开最近消息" : "折叠最近消息"} aria-expanded={!messagesCollapsed} on:click={toggleMessages}>{#if messagesCollapsed}<PanelRightOpen size={14} />{:else}<PanelRightClose size={14} />{/if}</button>
          </div></div>
          <div class="message-panel-body" hidden={messagesCollapsed}>
          {#if showDateFilter || showProductFilter}<div class="message-filter-fields">{#if showDateFilter}<input aria-label="日期" type="date" bind:value={messageDate} on:input={() => void resetMessageQuery()} />{/if}{#if showProductFilter}<select aria-label="商品" bind:value={messageProduct} on:change={() => void resetMessageQuery()}><option value="">全部商品</option>{#each messageProducts as [id, name]}<option value={id}>{name}</option>{/each}</select>{/if}</div>{/if}
          {#if messageHasUpdates}<button class="text-button" on:click={() => void resetMessageQuery()}>有新消息，刷新列表</button>{/if}
          {#if messageError}<button class="product-error-link" on:click={() => void loadMessages()}>读取消息失败：{messageError}。点击重试</button>{/if}
          {#if visibleMessages.length}
            <div class="message-scroll" bind:this={messageScroll}><table class="message-table"><thead class="sr-only"><tr><th>时间</th><th>商品</th><th>是否上架</th><th>库存</th><th>消息</th></tr></thead><tbody style:height={`${$messageVirtualizer.getTotalSize()}px`}>
              {#each $messageVirtualizer.getVirtualItems() as row (row.key)}
                {@const item = visibleMessages[row.index]}
                <tr style:transform={`translateY(${row.start}px)`}><td title={timePrecise(item.at)}><span class="message-day">{messageDay(item.at)}</span><time class="message-clock" datetime={item.at}>{messageClock(item.at)}</time></td><td title={item.name}>{item.name}</td><td>{item.isShow === null ? "—" : item.isShow === 1 ? "已上架" : "未上架"}</td><td>{item.stock ?? "—"}</td><td title={item.detail}>{item.detail}</td></tr>
              {/each}
            </tbody></table></div>
          {:else}<p class="empty">{messageLoading ? "正在读取消息…" : messageDate || messageProduct ? "没有符合筛选条件的消息。" : "库存或上架状态变化后，消息会显示在这里。"}</p>{/if}
          {#if messagePage > 0 || messageNextCursor}<div class="message-pagination"><button class="text-button" disabled={messagePage === 0 || messageLoading} on:click={() => void changeMessagePage(messagePage - 1)}>上一页</button><span>第 {messagePage + 1} 页</span><button class="text-button" disabled={!messageNextCursor || messageLoading} on:click={() => void changeMessagePage(messagePage + 1)}>下一页</button></div>{/if}
          </div>
        </section></div>
        <div class="product-management-link"><button class="text-button" on:click={() => navigate("products")}>管理商品</button></div>
      </section>
    {:else if page === "notifications"}
      <section class="content">
        <section class="group">
          <div class="system-notification-row">
            <h2>系统通知</h2>
            <label class="switch"><span>启用</span><input aria-label="启用系统通知" type="checkbox" disabled={pendingToggles.has("system-notifications")} checked={snapshot?.systemNotificationsEnabled ?? false} on:change={(event) => saveToggle("system-notifications", event.currentTarget, snapshot?.systemNotificationsEnabled ?? false, (enabled) => desktopApi.setSystemNotificationsEnabled(enabled))} /></label>
            <small>权限：{snapshot?.platform?.notificationPermission === "granted" ? "已允许" : snapshot?.platform?.notificationPermission === "denied" ? "已关闭" : snapshot?.platform?.notificationPermission === "unavailable" ? "暂时无法读取" : snapshot?.platform?.notificationPermission === "prompt" ? "首次发送时询问" : snapshot?.platform?.notificationPermission === "prompt_with_rationale" ? "需要授权" : "尚未确认"}</small>
            {#if snapshot?.platform?.notificationSettingsAvailable}<button class="secondary" disabled={busy} on:click={() => checkNotificationPermission(true)}>系统通知设置</button>{/if}
            {#if snapshot?.platform?.notificationPermission === "prompt" || snapshot?.platform?.notificationPermission === "prompt_with_rationale"}<button class="secondary" disabled={busy} on:click={() => checkNotificationPermission(true)}>申请权限</button>{/if}
            <button class="secondary" disabled={busy} on:click={() => checkNotificationPermission()}>检查权限</button>
            <button class="secondary" disabled={busy} on:click={sendSystemTest}>发送测试</button>
          </div>
          {#if snapshot?.platform?.notificationPermission === "denied" && !snapshot.systemNotificationsEnabled}<p class="quiet">请在系统设置中开启本应用通知，返回后点击“检查权限”。</p>{/if}
          {#if snapshot?.systemNotificationDelivery}
            <p class="muted">最近系统通知：{snapshot.systemNotificationDelivery.outcome === "accepted" ? "已提交" : snapshot.systemNotificationDelivery.outcome === "unknown" ? "尚未确认送达" : "发送失败"} · {snapshot.systemNotificationDelivery.message}</p>
          {/if}
          <div class="prominent-test-row"><p class="quiet">突出提醒会在商品上架或补货时覆盖屏幕，可通过商品旁的图标开启。</p><button class="secondary" disabled={busy || !productRows.length} on:click={testProminentAlert}>测试突出提醒</button></div>
        </section>
        <section class="group">
          <div class="section-heading channel-heading"><h2>通知渠道</h2><ChannelAdd providers={snapshot?.providers ?? []} {busy} onAdd={(id) => editChannel(null, id)} /></div>
          {#if !snapshot?.channels.length}<p class="empty channel-empty">尚未添加通知渠道。添加后可向所选会话发送库存提醒。</p>{/if}
          {#each snapshot?.channels ?? [] as item}
            <article class="channel-row">
              <ProviderIcon providerId={item.providerId} name={item.providerName} size={32} />
              <div class="channel-summary"><div class="channel-title"><strong title={item.name}>{item.name}</strong><ChannelStatus channel={item} testing={pendingTests.has(item.id)} deliveryDescription={item.lastDelivery ? `最近投递：${deliveryOutcome(item.lastDelivery.outcome)} · ${deliveryEvent(item.lastDelivery.event)} · ${time(item.lastDelivery.at)}` : ""} /></div><div class="channel-recipients" role="group" aria-label={`${item.name}接收对象`}>{#each item.selectedTargets ?? [] as target, index (`${target.kind}:${target.id}`)}<RecipientChip {target} {index} delivery={item.recipientDeliveries?.find((recipient) => recipient.target.kind === target.kind && recipient.target.id === target.id)?.lastDelivery ?? null} />{:else}<small>尚未选择接收对象</small>{/each}</div></div>
              <div class="channel-actions">
                <button class="icon-button" aria-label={`测试${item.name}`} title="发送测试消息" disabled={pendingTests.has(item.id)} on:click={() => testChannel(item.id)}><Send size={16} /></button>
                <button class="icon-button" aria-label={`编辑${item.name}`} title="编辑" on:click={() => editChannel(item)}><Pencil size={16} /></button>
                {#if item.connectionStatus === "auth_required"}<button class="secondary" on:click={() => editChannel(item)}>重新授权</button>{/if}
                <button class="icon-button" disabled={!item.enabled && (item.lastTest?.outcome !== "accepted" || item.connectionStatus === "auth_required")} aria-label={`${item.enabled ? "停用" : "启用"}${item.name}`} title={item.enabled ? "停用" : item.lastTest?.outcome === "accepted" ? "启用" : "测试成功后启用"} on:click={() => run(() => desktopApi.setChannelEnabled(item.id, !item.enabled))}>{#if item.enabled}<Pause size={16} />{:else}<Play size={16} />{/if}</button>
                <button class="icon-button danger" aria-label={`删除${item.name}`} title="删除" on:click={() => askRemoveChannel(item.id)}><Trash2 size={16} /></button>
              </div>
              {#if channelToDeleteId === item.id}
                <div bind:this={deleteConfirmation} class="channel-delete-confirm" role="group" aria-label={`确认删除${item.name}`} tabindex="-1">
                  <span>确定删除通知渠道“{item.name}”吗？已保存的凭据也会永久删除，无法恢复。</span>
                  <button class="secondary" on:click={() => channelToDeleteId = null}>取消删除</button>
                  <button class="danger-button" disabled={busy} on:click={() => removeChannel(item.id)}>确认删除</button>
                </div>
              {/if}
            </article>
          {/each}
        </section>
      </section>
    {:else if page === "rate"}
      <section class="content">
        <div class="section-heading"><h2>检查计划</h2><div class="actions">{#if rateDirty}<button class="text-button" on:click={() => resetConfig("rate")}>重置更改</button>{/if}<button class="primary" disabled={busy || !rateDirty} on:click={() => saveConfig("rate")}>保存设置</button></div></div>
        <section class="group"><ScheduleFields schedule={config.schedule} target="config" onTime={(key, value) => editSchedule({ [key]: value })} onDay={toggleDay} /></section>
        <section class="group"><h2>检查频率</h2><RateFields rate={config.rate} monitoringMode={config.monitoringMode} target="config" onChange={editRate} /></section>
        <section class="group"><h2>失败提醒通知</h2><label class="field">连续失败达到（分钟）<input type="number" min="1" step="1" value={config.failureAlertAfterMinutes} on:input={(event) => editConfig({ failureAlertAfterMinutes: Number(event.currentTarget.value) })} /></label></section>
      </section>
    {:else if page === "proxies"}
      <section class="content">
        <section class="group">
          <h2>网络连接</h2>
          <p class="quiet proxy-guidance">一般用户无需使用代理池。已开启系统代理时，可按网络需要启用“使用系统代理”。</p>
          <label class="switch"><span>使用系统代理</span><input type="checkbox" checked={config.useSystemProxy} on:change={(event) => editConfig({ useSystemProxy: event.currentTarget.checked, useProxyPool: false })} /></label>
          <label class="switch"><span>使用代理池</span><input type="checkbox" checked={config.useProxyPool} on:change={(event) => editConfig({ useProxyPool: event.currentTarget.checked, useSystemProxy: false })} /></label>
          <p class="quiet">默认直连理光接口。仅在直连不可用时启用系统代理；启用代理池后，监控请求使用下方已启用的代理。</p>
          <div class="actions">{#if networkDirty}<button class="text-button" on:click={() => resetConfig("network")}>重置更改</button>{/if}<button class="primary" disabled={busy || !networkDirty} on:click={() => saveConfig("network")}>保存设置</button></div>
        </section>
        <section class="group"><div class="section-heading"><h2>代理列表</h2><button class="secondary" aria-label="添加代理" title="添加代理" on:click={openProxyDialog}><Plus size={16} />添加代理</button></div>
          {#if !(snapshot?.proxies.length)}<p class="empty">暂无代理</p>{/if}
          {#each snapshot?.proxies ?? [] as item}
            <article class="record proxy-list-row"><div><strong>{item.displayAddress}</strong><p>{({ available: "可用", cooldown: "冷却中", auto_disabled: "因连续失败自动停用", manually_disabled: "已手动停用", untested: "尚未测试" } as Record<ProxyRecord["status"], string>)[item.status]}</p></div><div class="actions"><button class="text-button" on:click={() => run(() => desktopApi.testProxy(item.id))}>测试</button><button class="text-button" on:click={() => run(() => desktopApi.setProxyEnabled(item.id, !item.enabled))}>{item.enabled ? "停用" : "启用"}</button><button class="text-button danger" aria-label={`删除代理${item.displayAddress}`} on:click={() => askRemoveProxy(item.id)}>删除</button></div>
              {#if proxyToRemoveId === item.id}
                <div bind:this={proxyRemoveConfirmation} class="channel-delete-confirm proxy-remove-confirm" role="group" aria-label={`确认删除代理${item.displayAddress}`} tabindex="-1">
                  <span>确定删除代理 {item.displayAddress} 吗？删除后需重新添加。</span>
                  <button class="secondary" on:click={() => proxyToRemoveId = null}>取消删除</button>
                  <button class="danger-button" disabled={busy} on:click={() => removeProxy(item.id)}>确认删除</button>
                </div>
              {/if}
            </article>
          {/each}
        </section>
      </section>
      {#if proxyDialogOpen}<dialog bind:this={proxyDialog} class="proxy-modal" aria-labelledby="proxy-dialog-title" on:close={() => proxyDialogOpen = false}>
        <div class="section-heading"><h2 id="proxy-dialog-title">添加代理</h2><button class="icon-button" aria-label="关闭" on:click={closeProxyDialog}><X size={17} /></button></div>
        <div class="modal-body">
        <div class="proxy-tabs" role="tablist" aria-label="添加方式" tabindex="-1" on:keydown={onProxyTabKeydown}><button id="proxy-single-tab" role="tab" aria-controls="proxy-single-panel" aria-selected={proxyTab === "single"} tabindex={proxyTab === "single" ? 0 : -1} class:active={proxyTab === "single"} on:click={() => proxyTab = "single"}>添加单个代理</button><button id="proxy-batch-tab" role="tab" aria-controls="proxy-batch-panel" aria-selected={proxyTab === "batch"} tabindex={proxyTab === "batch" ? 0 : -1} class:active={proxyTab === "batch"} on:click={() => proxyTab = "batch"}>批量导入</button></div>
        <div id="proxy-single-panel" role="tabpanel" aria-labelledby="proxy-single-tab" hidden={proxyTab !== "single"}><div class="form-grid"><label class="field">协议<select bind:value={proxy}><option value="http">HTTP</option><option value="https">HTTPS</option><option value="socks5">SOCKS5</option></select></label><label class="field">主机<input bind:value={proxyHost} /></label><label class="field">端口<input type="number" min="1" max="65535" bind:value={proxyPort} /></label><label class="field">用户名<input bind:value={proxyUsername} /></label><label class="field">密码<input type="password" bind:value={proxyPassword} /></label></div></div>
        <div id="proxy-batch-panel" role="tabpanel" aria-labelledby="proxy-batch-tab" hidden={proxyTab !== "batch"}><textarea rows="6" bind:value={proxyImport} placeholder="每行一条代理"></textarea></div>
        </div>
        <div class="modal-actions"><button class="secondary" on:click={closeProxyDialog}>取消</button>{#if proxyTab === "single"}<button class="primary" disabled={busy} on:click={addProxy}>添加代理</button>{:else}<button class="primary" disabled={busy || !proxyImport.trim()} on:click={importProxies}>导入</button>{/if}</div>
      </dialog>{/if}
    {:else if page === "products"}
      <section class="content product-content" class:drawer-open={!!selectedProduct}>
        <section class="group product-management"><div class="section-heading product-list-heading"><div><h2>监控产品</h2><p>{productRows.length} 个商品 · 已监控 {productRows.filter((item) => item.enabled).length} 个</p></div><div class="actions"><button class="text-button product-lookup-toggle" aria-expanded={productLookupOpen} aria-controls="product-lookup-tools" on:click={() => productLookupOpen = !productLookupOpen}>查找商城商品</button><div class="product-search"><span class="search-icon"><Search size={16} aria-hidden="true" /></span><input aria-label="搜索名称或 Product ID" bind:value={productSearch} placeholder={'如"官翻品" "GR III"'} />{#if productSearch}<button class="icon-button" aria-label="清除商品搜索" on:click={() => productSearch = ""}><X size={14} /></button>{/if}</div><select aria-label="商品筛选" bind:value={productFilter}><option value="all">全部</option><option value="selected">监控中</option><option value="unselected">未监控</option><option value="prominent">突出提醒</option></select></div></div>
          {#if productLookupOpen}<div class="product-toolbar" id="product-lookup-tools"><div class="toolbar-group"><input class="product-id-input" aria-label="Product ID" bind:value={productId} inputmode="numeric" placeholder="Product ID" on:keydown={(event) => event.key === "Enter" && addProduct()} /><button class="secondary" aria-label="添加商品" disabled={busy || !productId.trim()} on:click={() => addProduct()}>添加</button></div><span class="toolbar-divider" aria-hidden="true"></span><div class="toolbar-group"><span class="quiet">编号范围</span><input class="range-input" aria-label="从" bind:value={scanStart} inputmode="numeric" placeholder="从" /><span>–</span><input class="range-input" aria-label="到" bind:value={scanEnd} inputmode="numeric" placeholder="到" /><button class="secondary" aria-label="开始扫描" disabled={busy || snapshot?.scan?.status === "running"} on:click={validateAndScan}>扫描</button></div></div>{/if}
          {#if snapshot?.scan && !scanResultDismissed}<div class="scan-status"><span>{snapshot.scan.status === "running" ? "扫描中" : snapshot.scan.status === "completed" ? "扫描完成" : snapshot.scan.status === "paused" ? "已暂停" : "已停止"} · {snapshot.scan.currentId ?? "—"}（{snapshot.scan.startId}–{snapshot.scan.endId}） · 找到 {snapshot.scan.found}</span>{#if snapshot.scan.error}<span class="inline-error">{snapshot.scan.error}</span>{/if}<div class="actions">{#if snapshot.scan.status === "running"}<button class="text-button" on:click={() => scanAction("pause")}>暂停</button>{:else if snapshot.scan.status === "paused"}<button class="text-button" on:click={() => scanAction("resume")}>继续</button>{/if}{#if snapshot.scan.status === "running" || snapshot.scan.status === "paused"}<button class="text-button danger" on:click={() => scanAction("cancel")}>取消</button>{:else}<button class="icon-button" aria-label="关闭扫描结果" title="关闭" on:click={() => scanResultDismissed = true}><X size={16} /></button>{/if}</div></div>{/if}
          {#if !productRows.length}<p class="empty">暂无商品，输入 Product ID 验证后添加。</p>{/if}
          {#if productRows.length && !visibleProducts.length}<p class="empty">没有符合条件的商品。</p>{/if}
          <div class="product-choice-grid">{#each visibleProducts as item (item.productId)}
            <ProductChoice product={item} selected={item.enabled} pending={pendingToggles.has(`product:${item.productId}`)} onToggle={(enabled) => toggleProduct(item.productId, enabled)} onOpen={(origin) => openProductDetail(item, origin)}>
              <div class="choice-meta"><span>ID {item.productId}</span>{#if item.metadata?.price}<span>¥{item.metadata.price}</span>{/if}</div>
              <svelte:fragment slot="actions">
                {#if item.enabled}<button class="icon-button prominent-toggle" class:enabled={item.prominentAlert} aria-label={`突出提醒${item.name}`} aria-pressed={item.prominentAlert} title={item.prominentAlert ? "关闭突出提醒" : "开启突出提醒"} disabled={pendingToggles.has(`prominent:${item.productId}`)} on:click={() => toggleProminent(item.productId, !item.prominentAlert)}><ProminentIcon /></button>{/if}
                <button class="icon-button" aria-label={`查看${item.name}详情`} on:click={(event) => openProductDetail(item, event.currentTarget)}><Info size={14} /></button>
              </svelte:fragment>
            </ProductChoice>
          {/each}</div>
        </section>
      </section>
    {:else if page === "settings"}
      <section class="content">
        <section class="group"><h2>监控方案</h2>
          <fieldset class="monitoring-modes" aria-label="监控方案">
            <label class="mode-choice" class:chosen={config.monitoringMode === "listed_products"}>
              <div class="mode-choice-title"><input type="radio" name="monitoring-mode" aria-label="全站上架列表（推荐）" checked={config.monitoringMode === "listed_products"} on:change={() => editConfig({ monitoringMode: "listed_products" })} /><strong>全站上架列表（推荐）</strong></div>
              <p>一轮分页检查全站上架列表，同步所有监控商品。</p>
              <h3>优点</h3><ul><li>一次完整检查覆盖所有监控商品。</li><li>监控商品较多时，请求量较少。</li></ul>
              <h3>缺点</h3><ul><li>需要等分页检查完成后更新。</li><li>未上架商品的库存无法获取。</li></ul>
            </label>
            <label class="mode-choice" class:chosen={config.monitoringMode === "product_detail"}>
              <div class="mode-choice-title"><input type="radio" name="monitoring-mode" aria-label="逐商品详情" checked={config.monitoringMode === "product_detail"} on:change={() => editConfig({ monitoringMode: "product_detail" })} /><strong>逐商品详情</strong></div>
              <p>逐商品请求详情，分别检查各监控商品。</p>
              <h3>优点</h3><ul><li>单个商品检查完成即更新。</li><li>适合重点关注少量商品。</li></ul>
              <h3>缺点</h3><ul><li>请求量随监控商品数量成倍增加。</li><li>增加网络负担和请求失败风险。</li></ul>
            </label>
          </fieldset>
          <div class="mode-setting-footer"><button class="primary" disabled={busy || !modeDirty} on:click={() => saveConfig("mode")}>保存设置</button></div>
        </section>
        <section class="group settings-group"><h2>应用</h2>
          <div class="settings-row"><div><strong>{appInfo.productName}</strong><p>版本 {appVersion}</p></div></div>
          <div class="settings-row"><div><strong>帮助引导</strong><p>重新选择监控商品、设置通知与登录后启动。</p></div><button class="secondary" on:click={() => { resetConfig(); navigate("setup"); }}>帮助引导</button></div>
          <div class="settings-row"><div><strong>外观</strong><p>跟随系统时，外观随系统设置自动切换。</p></div><select aria-label="外观" value={theme} on:change={(event) => changeTheme(event.currentTarget.value as ThemePreference)}><option value="system">跟随系统</option><option value="light">浅色</option><option value="dark">深色</option></select></div>
          <div class="settings-row"><div><strong>登录后启动</strong><p>登录系统后自动打开应用。</p></div><label class="switch"><input aria-label="登录后启动" type="checkbox" disabled={pendingToggles.has("login-start")} checked={snapshot?.platform?.loginStartEnabled ?? false} on:change={(event) => saveToggle("login-start", event.currentTarget, snapshot?.platform?.loginStartEnabled ?? false, (enabled) => desktopApi.setLoginStart(enabled))} /></label></div>
          <div class="settings-row"><div><strong>启动后自动开启监控</strong><p>打开应用后按监控计划运行。</p></div><label class="switch"><input aria-label="启动后自动开启监控" type="checkbox" disabled={pendingToggles.has("auto-monitor")} checked={snapshot?.config?.autoStartMonitoring ?? true} on:change={(event) => saveToggle("auto-monitor", event.currentTarget, snapshot?.config?.autoStartMonitoring ?? true, (enabled) => desktopApi.setAutoStartMonitoring(enabled))} /></label></div>
          <div class="settings-row"><div><strong>日志目录</strong><p>查看应用运行记录。</p></div><button class="secondary" on:click={() => run(() => desktopApi.openLogsDirectory())}>打开目录</button></div>
          <UpdateNotice mode="settings" status={updateStatus} checked={updateChecked} onCheck={checkUpdates} onInstall={installUpdate} />
        </section>
        {#if snapshot?.platform && (snapshot.platform.projectUrl || snapshot.platform.tutorialUrl || snapshot.platform.feedbackUrl)}<section class="group settings-group"><h2>相关链接</h2>{#each [{ key: "projectUrl", label: "项目主页" }, { key: "tutorialUrl", label: "使用教程" }, { key: "feedbackUrl", label: "反馈" }] as item}{#if snapshot.platform[item.key as keyof typeof snapshot.platform]}<button class="link-row" on:click={() => openAppLink(item.key as "projectUrl" | "tutorialUrl" | "feedbackUrl")}>{item.label} <span aria-hidden="true">↗</span></button>{/if}{/each}</section>{/if}
        <section class="group settings-group"><h2>数据与诊断</h2>
          <div class="settings-row"><div><strong>配置文件</strong><p>导入会合并商品和通知渠道，并按文件更新监控设置；导入后监控停止。导出不含通知凭据，导入渠道需重新测试后启用。</p></div><div class="actions"><button class="secondary" disabled={busy} on:click={() => run(async () => { if (await desktopApi.exportConfigurationFile()) notice = { kind: "success", text: "配置已导出。" }; })}>导出配置…</button><button class="secondary" disabled={busy} on:click={importConfigFile}>导入配置…</button></div></div>
          <div class="settings-row"><div><strong>诊断报告</strong><p>生成当前运行信息以便排查问题。</p></div><button class="secondary" on:click={() => run(async () => { diagnostic = await desktopApi.createDiagnosticPreview(); })}>生成预览</button></div>
          {#if diagnostic}<div class="diagnostic-preview"><textarea readonly rows="8" bind:value={diagnostic}></textarea><button class="secondary" on:click={() => run(() => desktopApi.saveDiagnosticReport(diagnostic))}>保存报告</button></div>{/if}
        </section>
        <section class="group settings-group"><h2>恢复</h2><div class="settings-row"><div><strong>恢复默认设置</strong><p>移除已添加的商品、渠道和代理。</p></div>{#if !resetOpen}<button class="danger-button" on:click={() => resetOpen = true}>恢复默认设置…</button>{/if}</div>{#if resetOpen}<div class="reset-confirm"><label><input type="checkbox" bind:checked={clearHistory} />同时删除历史记录</label><div class="actions"><button class="secondary" on:click={() => resetOpen = false}>取消</button><button class="danger-button" disabled={busy} on:click={restoreDefaults}>确认恢复</button></div></div>{/if}</section>
      </section>
    {/if}
      {#if channelFormOpen}
          <dialog bind:this={channelDialog} class="channel-modal" class:configured={channelFormExpanded} aria-labelledby="channel-form-title" on:close={closeChannel} on:cancel={(event) => { if (savingChannel) event.preventDefault(); else closeChannel(); }}>
            <div class="section-heading"><h2 id="channel-form-title">{channel ? "编辑" : "添加"}{currentProvider?.name}</h2><button class="icon-button" aria-label="关闭" disabled={savingChannel} on:click={closeChannel}><X size={17} /></button></div>
            <div class="modal-body">
            {#if !currentProvider?.supportsBinding || channelMode === "manual"}<label class="field">渠道名称<input aria-label="渠道名称" bind:value={channelName} placeholder={defaultChannelName} /></label>{/if}
            {#if currentProvider?.supportsBinding}
              {#if channelMode === "binding"}<ChannelBinding bind:this={channelBindingEditor} {providerId} providerName={currentProvider.name} existing={editingChannel} rebindOnMount={channelRebindOnMount} defaultName={defaultChannelName} bind:connected={channelConnected} bind:channelName bind:binding={channelBinding} bind:targetId={channelTargetId} bind:targetKind={channelTargetKind} bind:selectedTargets={channelSelectedTargets} />{/if}
            {/if}
            {#if currentProvider?.documentationUrl && (!currentProvider.supportsBinding || channelMode === "manual")}<button class="link-row" on:click={openProviderDocumentation}>查看{currentProvider.name}机器人配置说明 ↗</button>{/if}
            {#each currentProvider?.supportsBinding && channelMode === "binding" ? [] : currentProvider?.fields ?? [] as field}
              <label class="field">{field.label}{#if field.required} *{/if}
                <input aria-label={field.label} type={field.type === "password" ? "password" : "text"} value={channelValues[field.key] ?? ""} placeholder={channel?.configuredFieldKeys.includes(field.key) ? "留空保留原值" : field.placeholder ?? ""} on:input={(event) => channelValues = { ...channelValues, [field.key]: event.currentTarget.value }} />
                {#if field.help}<small>{field.help}</small>{/if}
              </label>
            {/each}
            {#if channelError}<p class="notice error" role="alert">{channelError}</p>{/if}
            </div>
            <footer class="channel-footer">
            {#if !currentProvider?.supportsBinding || channelMode === "manual" || channelConnected}
            <fieldset class="subscriptions"><legend>提醒内容</legend><div class="subscription-options">{#each events as event}<label><input type="checkbox" checked={channelEvents.includes(event.id)} on:change={() => toggleEvent(event.id)} />{event.label}</label>{/each}</div></fieldset>
            {/if}
            <div class="modal-actions">{#if currentProvider?.supportsBinding}<div class="channel-action-links"><button class="text-button" disabled={savingChannel} on:click={() => { channelMode = channelMode === "binding" ? "manual" : "binding"; channelRebindOnMount = channelMode === "binding"; channelBinding = null; channelConnected = false; channelError = ""; }}>{channelMode === "binding" ? "手动配置" : "扫码连接"}</button>{#if channelMode === "binding" && channelConnected}<button class="text-button" disabled={savingChannel} on:click={() => channelBindingEditor.restart()}>重新扫码</button>{/if}</div>{/if}<button class="secondary" disabled={savingChannel} on:click={closeChannel}>取消</button><button class="primary" disabled={busy || channelFirstMessagePending || !!currentProvider?.supportsBinding && channelMode === "binding" && (!channelConnected || !channelSelectedTargets.length)} on:click={() => saveChannel()}>保存</button></div>
            </footer>
          </dialog>
      {/if}
    {#if selectedProduct}
      <div class="product-detail-drawer" role="dialog" aria-label="商品详情">
        <div class="drawer-heading"><h2>商品详情</h2><button bind:this={detailCloseButton} class="icon-button" aria-label="关闭商品详情" on:click={() => closeProductDetail()}><X size={20} /></button></div>
        {#if productImageSrc(selectedProduct)}<div class="drawer-visual"><img class="drawer-photo" src={productImageSrc(selectedProduct) ?? ""} alt={selectedProduct.name} /></div>{/if}
        <h3>{selectedProduct.name}</h3>
        <div class="drawer-state"><span>{productState(selectedProduct)}</span><button class="text-button danger" disabled={pendingToggles.has(`product:${selectedProduct.productId}`)} on:click={() => toggleProduct(selectedProduct.productId, !selectedProduct.enabled)}>{selectedProduct.enabled ? "取消监控" : "添加监控"}</button></div>
        {#if selectedProduct.enabled}<button class="secondary prominent-toggle drawer-prominent" class:enabled={selectedProduct.prominentAlert} aria-label="商品详情突出提醒" aria-pressed={selectedProduct.prominentAlert} title={selectedProduct.prominentAlert ? "关闭突出提醒" : "开启突出提醒"} disabled={pendingToggles.has(`prominent:${selectedProduct.productId}`)} on:click={() => toggleProminent(selectedProduct.productId, !selectedProduct.prominentAlert)}><ProminentIcon /><span>突出提醒</span><small>{selectedProduct.prominentAlert ? "已开启" : "未开启"}</small></button>{/if}
        <section class="drawer-section"><h4>检查记录</h4><p>今日检查 {selectedProduct.todayCheckCount ?? 0} 次</p><p>成功 {selectedProduct.todaySuccessCount ?? 0} · 失败 {selectedProduct.todayFailureCount ?? 0}</p><p>累计检查 {selectedProduct.checkCount} 次</p><p>累计监控 {monitoringDuration(selectedProduct.monitoringMs)}</p><p>最近成功 {timePrecise(selectedProduct.observation?.checkedAt)}</p></section>
        <section class="drawer-section"><h4>最近状态记录</h4>{#if detailLoading}<p>正在读取…</p>{:else if detailMessageError}<button class="error-link" on:click={() => openProductDetail(selectedProduct!, detailOrigin)}>读取失败：{detailMessageError}。重试</button>{:else if detailMessages.length}<div class="drawer-records">{#each detailMessages as item}<div><time>{timePrecise(item.at)}</time><span>{item.detail}</span></div>{/each}</div>{:else}<p>暂无状态记录。</p>{/if}</section>
        {#if selectedProduct.runtimeError}<section class="drawer-section"><h4>检查失败</h4><pre class="choice-error">{selectedProduct.runtimeError}</pre></section>{/if}
        <section class="drawer-section"><h4>商品信息</h4>{#if selectedProduct.metadata?.price !== null && selectedProduct.metadata?.price !== undefined}<p>价格 ¥{selectedProduct.metadata.price}{selectedProduct.metadata.unitName ? ` / ${selectedProduct.metadata.unitName}` : ""}</p>{/if}{#if selectedProduct.metadata?.productNo}<p>商品编号 {selectedProduct.metadata.productNo}</p>{/if}{#if selectedProduct.metadata?.isMemberCard}<p>会员卡{selectedProduct.metadata.memberCardDays ? ` · ${selectedProduct.metadata.memberCardDays} 天` : ""}</p>{/if}{#if selectedProduct.metadataUpdatedAt}<p>更新于 {time(selectedProduct.metadataUpdatedAt)}</p>{/if}</section>
        {#if page === "products"}<section class="drawer-section"><button class="text-button danger" disabled={busy || pendingToggles.has(`product:${selectedProduct.productId}`)} on:click={() => requestProductRemoval(selectedProduct!)}>删除商品</button>
          {#if productToRemoveId === selectedProduct.productId}<div bind:this={productDeleteConfirmation} class="channel-delete-confirm" role="group" aria-label={`确认删除${selectedProduct.name}`} tabindex="-1"><span>此商品正在监控。删除后将停止监控并从列表移除。</span><button class="secondary" disabled={busy} on:click={() => productToRemoveId = null}>取消</button><button class="danger-button" disabled={busy} on:click={() => removeProduct(selectedProduct!.productId)}>确认删除</button></div>{/if}
        </section>{/if}
      </div>
    {/if}
    <dialog bind:this={detailDialog} class="product-error-modal" aria-labelledby="error-detail-title">
      <div class="section-heading"><h2 id="error-detail-title">{detailTitle}</h2><button class="icon-button" aria-label="关闭" on:click={() => detailDialog.close()}><X size={17} /></button></div>
      <pre>{detailError}</pre>
    </dialog>
    {#if prominentPromptProduct}<dialog bind:this={prominentPromptDialog} class="product-error-modal" aria-labelledby="prominent-prompt-title" on:close={() => prominentPromptProduct = null} on:cancel={(event) => { if (busy) event.preventDefault(); else closeProminentPrompt(); }}>
      <h2 id="prominent-prompt-title">先体验突出提醒</h2>
      <p>{prominentPromptProduct.name}</p>
      <p>突出提醒会覆盖当前屏幕。请先测试一次，了解提醒界面；按 Esc、空格或回车即可关闭。</p>
      <div class="modal-actions"><button class="secondary" disabled={busy} on:click={closeProminentPrompt}>取消</button><button class="primary" disabled={busy} on:click={confirmProminentTest}>测试</button></div>
    </dialog>{/if}
  </main>
</div>
{#if previewAlertOpen}<div class="preview-alert" role="dialog" aria-label="突出提醒测试"><iframe title="突出提醒演示" src="/prominent-alert"></iframe><button class="preview-alert-close" aria-label="关闭突出提醒测试" on:click={() => previewAlertOpen = false}><X size={18} /></button></div>{/if}

<style>
  :global(*) { box-sizing: border-box; }
  :global(body) { margin: 0; color: var(--foreground); background: var(--canvas); font: 15px/1.45 -apple-system, BlinkMacSystemFont, "PingFang SC", "Helvetica Neue", sans-serif; -webkit-font-smoothing: antialiased; font-variant-numeric: tabular-nums; }
  :global(::selection) { color: #fff; background: #a2252d; }
  :global(input), :global(textarea) { caret-color: var(--accent); }
  :global(button), :global(input), :global(select), :global(textarea) { font: inherit; }
  :global(button) { cursor: pointer; }
  :global(button:disabled) { cursor: default; }
  :global(:focus-visible) { outline: 2px solid var(--accent); outline-offset: 2px; }
  .shell { height: 100dvh; display: grid; grid-template-columns: 210px minmax(0, 1fr); grid-template-rows: minmax(0, 1fr); }
  .shell.onboarding-shell { grid-template-columns: minmax(0, 1fr); }
  .shell.onboarding-shell main { display: flex; flex-direction: column; padding: 0; height: 100dvh; overflow: hidden; }
  .shell.onboarding-shell main > .notice:not(.toast) { flex-shrink: 0; margin: 12px 24px 0; }
  .sidebar { min-height: 0; display: flex; flex-direction: column; padding: 22px 10px 14px; background: var(--sidebar); border-right: 1px solid var(--border); overflow: hidden; }
  .brand-signature { display: flex; flex-direction: column; align-items: flex-start; justify-content: center; gap: 10px; flex: none; min-height: 94px; margin: 0 16px 16px; }
  .brand-wordmark { display: block; width: 90px; height: auto; }
  .brand-name { font-size: 12px; line-height: 1.4; color: var(--muted); white-space: nowrap; letter-spacing: -.025em; }
  .preview-label { margin: 0 12px 18px; padding: 8px 10px; color: var(--muted); background: var(--subtle); border-radius: 4px; font-size: 12px; }
  nav { min-height: 0; overflow-y: auto; display: grid; align-content: start; gap: 2px; }
  nav button { display: flex; align-items: center; gap: 14px; min-height: 44px; padding: 0 22px; border: 0; border-radius: 5px; background: transparent; color: var(--foreground); text-align: left; font-size: 15px; font-weight: 450; transition: background 160ms var(--ease-ui), color 160ms var(--ease-ui); }
  nav button:hover { background: var(--subtle); color: var(--foreground); }
  nav button:active { background: var(--accent-soft); }
  nav button.active { color: var(--accent-text); background: var(--accent-soft); font-weight: 600; }
  nav button.active :global(svg) { color: var(--accent); }
  .sidebar-state { display: flex; align-items: center; justify-content: space-between; flex: none; margin: 0 2px; padding: 8px 6px 0 12px; color: var(--muted); font-size: 12px; border-top: 1px solid var(--border); }
  .theme-switch { color: var(--muted); }
  .sidebar-state i { display: inline-block; width: 7px; height: 7px; margin-right: 8px; border-radius: 50%; background: var(--muted); }
  .sidebar-state i.running { background: var(--positive); }
  main { position: relative; min-width: 0; min-height: 0; height: 100%; overflow-y: auto; width: 100%; padding: 32px 26px 40px; background: var(--canvas); }
  main.status-page::before { content: ""; position: absolute; inset: 0 0 auto; aspect-ratio: 2161 / 728; background: url('/brand/header-camera.webp') top right / 100% 100% no-repeat; opacity: var(--camera-opacity); mask-image: linear-gradient(to bottom, #000 0%, #000 35%, transparent 100%); pointer-events: none; }
  main > * { position: relative; }
  h2 { margin: 0 0 16px; font-size: 17px; line-height: 1.4; font-weight: 600; letter-spacing: -.02em; }
  h3 { margin: 0 0 12px; font-size: 14px; font-weight: 600; }
  p { line-height: 1.5; }
  .content { display: grid; align-content: start; gap: 18px; }
  .quiet, small { color: var(--muted); }
  .quiet { font-size: 12px; }
  .group { min-width: 0; padding: 14px 16px; border: 1px solid var(--border); border-radius: 6px; background: var(--surface); }
  .status-products { padding: 0; overflow: hidden; }
  .status-products .text-button { display: inline-flex; align-items: center; gap: 5px; color: var(--foreground); }
  .group > :last-child { margin-bottom: 0; }
  .loading-state { max-width: 1000px; margin: 20px auto; color: var(--muted); }
  .status-line, .section-heading, .actions, .record { display: flex; align-items: center; gap: 12px; }
  .status-line, .section-heading { justify-content: space-between; }
  .section-heading { min-height: 36px; margin-bottom: 14px; }
  .content > .section-heading { position: sticky; top: 0; z-index: 2; margin-bottom: 0; background: var(--canvas); }
  .content > .section-heading::before { content: ""; position: absolute; inset: -32px 0 100%; background: var(--canvas); pointer-events: none; }
  .section-heading h2 { margin: 0; }
  .system-notification-row { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; }
  .system-notification-row h2 { margin: 0 auto 0 0; }
  .status-line { min-height: 72px; padding: 4px 0 8px; color: var(--foreground); }
  .status-overview { display: flex; align-items: flex-start; gap: 13px; }
  .status-indicator { flex: none; width: 8px; height: 8px; margin-top: 10px; border-radius: 50%; background: var(--muted); }
  .status-indicator.healthy { background: var(--positive); }
  .status-indicator.warning { background: var(--warning); }
  .status-line h2 { margin-bottom: 4px; font-size: 19px; }
  .status-line p { margin: 4px 0; color: var(--muted); font-size: 12px; }
  .record p { margin: 4px 0; color: var(--muted); }
  .status-line .product-error-link { color: var(--danger); }
  .message-tools { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .filter-button { display: inline-flex; align-items: center; justify-content: center; gap: 8px; min-width: 36px; max-width: 200px; min-height: 36px; padding: 6px 9px; color: var(--muted); background: transparent; border: 1px solid transparent; border-radius: 5px; font-size: 12px; }
  .filter-button:hover { color: var(--foreground); background: var(--subtle); }
  .filter-button[aria-pressed="true"] { color: var(--accent-text); background: var(--accent-soft); }
  .message-scroll { max-height: 360px; overflow: auto; border-top: 1px solid var(--border); }
  .message-pagination { display: flex; justify-content: flex-end; align-items: center; gap: 12px; padding-top: 10px; color: var(--muted); font-size: 12px; }
  .message-table { display: block; width: 100%; min-width: 560px; border-collapse: collapse; font-variant-numeric: tabular-nums; }
  .message-table thead.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
  .message-table tbody { display: block; position: relative; }
  .message-table tr { display: grid; grid-template-columns: minmax(160px, 1.2fr) minmax(70px, 1.3fr) minmax(74px, .85fr) minmax(44px, .5fr) minmax(70px, .8fr); width: 100%; height: 48px; }
  .message-table tbody tr { position: absolute; top: 0; left: 0; }
  .message-table th, .message-table td { height: 48px; padding: 6px 8px; border-bottom: 1px solid var(--hairline); text-align: left; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .message-table th { color: var(--muted); font-size: 12px; font-weight: 500; }
  .record { justify-content: space-between; padding: 12px 0; border-top: 1px solid var(--hairline); }
  .record strong { font-weight: 550; }
  .product-error-link { width: fit-content; padding: 0; border: 0; background: transparent; color: var(--danger); text-decoration: underline; text-underline-offset: 2px; text-align: left; }
  .product-error-modal { width: min(520px, calc(100vw - 32px)); max-height: calc(100vh - 32px); padding: 22px; border: 1px solid var(--border); border-radius: 7px; color: var(--foreground); background: var(--surface); box-shadow: 0 24px 60px rgb(0 0 0 / 45%); }
  .product-error-modal::backdrop { background: var(--modal-backdrop); }
  .product-error-modal pre { overflow: auto; margin: 16px 0 0; padding: 14px; border: 1px solid var(--border); border-radius: 4px; background: var(--subtle); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; font-size: 11px; line-height: 1.5; }
  .record > div:first-child { min-width: 0; }
  .channel-row { display: grid; grid-template-columns: 36px minmax(0, 1fr) auto; align-items: center; gap: 12px; padding: 12px 0; border-top: 1px solid var(--hairline); }
  .channel-summary { display: grid; gap: 7px; min-width: 0; }
  .channel-title { display: flex; align-items: center; gap: 9px; min-width: 0; }
  .channel-title strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .channel-summary small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .channel-summary .channel-recipients { display: flex; overflow: hidden; align-items: center; gap: 14px; min-height: 19px; }
  .channel-recipients :global(.recipient) { max-width: 180px; flex: 0 1 auto; }
  .channel-actions { display: flex; align-items: center; gap: 4px; }
  .channel-delete-confirm { grid-column: 1 / -1; display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 12px; border: 1px solid var(--danger-border); border-radius: 5px; background: var(--danger-soft); }
  .channel-delete-confirm span { flex: 1 1 220px; }
  .icon-button { display: inline-grid; width: 32px; height: 32px; place-items: center; padding: 0; border: 0; border-radius: 5px; color: var(--muted); background: transparent; }
  .icon-button:hover { background: var(--subtle); color: var(--foreground); }
  .channel-modal, .proxy-modal { width: min(520px, calc(100vw - 32px)); max-height: calc(100dvh - 32px); overflow: hidden; padding: 22px; border: 1px solid var(--border); border-radius: 7px; color: var(--foreground); background: var(--surface); box-shadow: 0 24px 60px rgb(0 0 0 / 45%); }
  .channel-modal { width: min(520px, calc(100vw - 32px)); height: min(500px, calc(100dvh - 32px)); box-sizing: border-box; }
  .channel-modal.configured { height: min(650px, calc(100dvh - 32px)); }
  .proxy-modal { height: min(460px, calc(100dvh - 32px)); }
  .channel-modal[open], .proxy-modal[open] { display: flex; flex-direction: column; }
  .channel-modal > .section-heading, .proxy-modal > .section-heading { flex: none; }
  .modal-body { min-height: 0; flex: 1; overflow-y: auto; padding: 4px 5px; }
  .channel-modal .modal-body { flex: 1; }
  .channel-modal::backdrop, .proxy-modal::backdrop { background: var(--modal-backdrop); }
  .channel-modal .modal-body > .field { margin: 12px 0; }
  .channel-modal .section-heading { margin-bottom: 14px; }
  .channel-action-links { display: flex; align-items: center; gap: 12px; margin-right: auto; }
  .channel-action-links button { font-size: 12px; }
  .channel-modal .link-row { padding: 8px 0; border-bottom: 0; font-size: 12px; }
  .channel-footer { flex: none; }
  .subscriptions { display: block; min-width: 0; margin: 12px 0 0; padding: 0 5px; border: 0; }
  .subscription-options { display: flex; flex-wrap: wrap; gap: 8px 14px; }
  .subscriptions legend { margin-bottom: 10px; padding: 0; font-size: 13px; font-weight: 550; }
  .subscriptions label { display: inline-flex; align-items: center; gap: 6px; }
  .modal-actions { display: flex; flex: none; flex-wrap: wrap; justify-content: flex-end; gap: 8px; margin-top: 12px; padding-top: 12px; border-top: 1px solid var(--hairline); }
  .channel-modal .modal-actions { padding-top: 0; border-top: 0; }
  .proxy-tabs { display: flex; gap: 4px; margin-bottom: 18px; border-bottom: 1px solid var(--border); }
  .proxy-tabs button { padding: 10px 13px; color: var(--muted); background: transparent; border: 0; border-bottom: 2px solid transparent; }
  .proxy-tabs button.active { color: var(--accent); border-bottom-color: var(--accent); font-weight: 600; }
  .proxy-modal .form-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .product-toolbar { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 14px 0 16px; padding: 14px; border: 1px solid var(--border); border-radius: 5px; background: var(--subtle); }
  .toolbar-group { display: inline-flex; align-items: center; gap: 8px; white-space: nowrap; }
  .product-search { position: relative; display: flex; align-items: center; width: 184px; }
  .product-search .search-icon { position: absolute; left: 10px; display: grid; place-items: center; color: var(--muted); pointer-events: none; }
  .product-management .product-list-heading .product-search input { width: 100%; padding-left: 34px; padding-right: 28px; }
  .product-search .icon-button { position: absolute; right: 4px; width: 22px; height: 22px; }
  .product-toolbar .product-id-input { width: 130px; flex: none; }
  .product-toolbar .range-input { width: 66px; flex: none; }
  .toolbar-divider { width: 1px; height: 24px; margin: 0 5px; background: var(--border); }
  .scan-status { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 6px 0 12px; padding: 10px 12px; color: var(--muted); background: var(--subtle); border-radius: 5px; }
  .scan-status .actions { margin-left: auto; }
  .proxy-list-row { flex-wrap: wrap; }
  .proxy-remove-confirm { flex: 1 0 100%; }
  .actions { flex-wrap: wrap; }
  .switch { display: inline-flex; align-items: center; gap: 8px; }
  .switch input, input[type="checkbox"], input[type="radio"] { accent-color: var(--accent); }
  .field { display: grid; gap: 6px; min-width: 0; font-weight: 500; }
  .field small { font-weight: 400; }
  .field input, .field select, input:not([type="checkbox"]):not([type="radio"]), select, textarea { min-width: 0; min-height: 36px; padding: 7px 10px; color: var(--foreground); background: var(--control); border: 1px solid var(--control-border); border-radius: 5px; color-scheme: inherit; }
  select { height: 36px; }
  input:not([type="checkbox"]):not([type="radio"]):hover, select:hover, textarea:hover { border-color: var(--control-hover); }
  input::placeholder, textarea::placeholder { color: var(--muted); opacity: 1; }
  textarea { display: block; width: 100%; margin: 10px 0; resize: vertical; }
  button.primary, button.secondary, button.danger-button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 36px; padding: 7px 12px; border: 1px solid var(--control-border); border-radius: 5px; color: var(--foreground); background: var(--button-bg); font-weight: 550; white-space: nowrap; transition: background 160ms var(--ease-ui), border-color 160ms var(--ease-ui), color 160ms var(--ease-ui), transform 160ms var(--ease-ui); }
  button.secondary:hover:not(:disabled) { background: var(--button-hover); border-color: var(--control-hover); }
  button.primary:active:not(:disabled), button.secondary:active:not(:disabled), button.danger-button:active:not(:disabled) { transform: translateY(1px); }
  button.primary { border-color: var(--accent); color: white; background: var(--primary-bg); }
  button.primary:hover:not(:disabled) { background: var(--accent-hover); border-color: var(--accent-hover); }
  button.primary:disabled { border-color: var(--primary-disabled-border); color: var(--primary-disabled-text); background: var(--primary-disabled-bg); }
  button.secondary:disabled, button.danger-button:disabled { border-color: var(--border); color: var(--disabled-text); background: var(--subtle); }
  button.danger-button, .danger { color: var(--danger); }
  button.danger-button:hover:not(:disabled) { background: var(--danger-soft); border-color: var(--danger-border); }
  .text-button { padding: 5px 2px; color: var(--accent); border: 0; background: transparent; font-weight: 550; }
  .text-button.danger { color: var(--danger); }
  .text-button:hover { text-decoration: underline; text-underline-offset: 2px; }
  .link-row { display: flex; justify-content: space-between; width: 100%; padding: 13px 0; color: var(--foreground); border: 0; border-bottom: 1px solid var(--hairline); background: transparent; text-align: left; }
  .link-row:hover { color: var(--accent); }
  .notice { display: flex; justify-content: space-between; gap: 12px; padding: 11px 13px; border: 1px solid var(--danger-border); border-radius: 5px; color: var(--danger); background: var(--danger-soft); }
  .toast { position: fixed; z-index: 20; top: 18px; right: 20px; width: min(380px, calc(100vw - 40px)); margin: 0; box-shadow: 0 8px 24px rgb(0 0 0 / 14%); }
  .notice.success { color: var(--success-text); border-color: var(--success-border); background: var(--success-bg); }
  .notice button { border: 0; background: transparent; }
  .inline-error { color: var(--danger); }
  .empty { padding: 22px 2px; color: var(--muted); }
  .channel-empty { text-align: center; }
  fieldset { display: flex; flex-wrap: wrap; gap: 10px 16px; margin: 14px 0; padding: 12px; border: 1px solid var(--border); border-radius: 5px; }
  legend { padding: 0 5px; font-weight: 600; }
  .form-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 12px; margin: 12px 0; }
  .settings-group { padding-top: 18px; padding-bottom: 6px; }
  .settings-group > h2 { margin-bottom: 2px; }
  .settings-row { display: flex; align-items: center; justify-content: space-between; gap: 18px; min-height: 48px; padding: 8px 0; border-bottom: 1px solid var(--hairline); }
  .settings-row > div:first-child { min-width: 0; }
  .settings-row strong { font-weight: 500; }
  .settings-row p { margin: 3px 0 0; color: var(--muted); font-size: 12px; }
  .settings-row .actions { justify-content: flex-end; }
  .settings-group > .settings-row:last-child, .settings-group > .link-row:last-child { border-bottom: 0; }
  .diagnostic-preview, .reset-confirm { padding: 12px 0 16px; }
  .reset-confirm { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .reset-confirm label { display: inline-flex; align-items: center; gap: 8px; }
  @media (min-width: 701px) and (max-width: 820px) {
    main { padding: 20px 18px 32px; }
    .content { gap: 12px; }
    .recent-messages { padding: 16px 18px; }
    .message-table th, .message-table td { padding-inline: 6px; }
    .message-table { min-width: 420px; font-size: 12px; }
    .message-table tr { grid-template-columns: minmax(142px, 1.1fr) minmax(80px, 1fr) 66px 38px 70px; }
  }
  @media (max-width: 700px) {
    .shell { display: block; height: auto; min-height: 100dvh; }
    .sidebar { min-height: auto; overflow: visible; padding: 10px 12px; border-right: 0; border-bottom: 1px solid var(--border); }
    .brand-signature { min-height: 44px; margin: 0 8px 8px; flex-direction: row; align-items: center; gap: 12px; }
    .brand-wordmark { width: 54px; }
    nav { display: flex; overflow-x: auto; }
    nav button { flex: 0 0 auto; gap: 8px; padding: 0 10px; }
    .sidebar-state { display: none; }
    main { height: auto; overflow: visible; padding: 20px 14px 32px; }
    .group { padding: 17px 16px; }
    .form-grid { grid-template-columns: 1fr 1fr; }
    .system-notification-row { flex-wrap: wrap; gap: 10px 14px; }
    .system-notification-row h2 { flex: 1 0 100%; }
    .channel-row { grid-template-columns: 32px minmax(0, 1fr) auto; }
    .settings-row { align-items: flex-start; }
    .settings-row .actions { justify-content: flex-start; }
    .product-toolbar { padding: 11px; }
    .toolbar-divider { display: none; }
  }
  @media (max-width: 530px) {
    .status-line { align-items: flex-start; gap: 14px; }
    .status-line > button { align-self: flex-start; }
    .channel-row { grid-template-columns: 32px minmax(0, 1fr); }
    .channel-actions { grid-column: 2; justify-content: flex-start; }
    .settings-row { flex-wrap: wrap; }
    .reset-confirm { align-items: flex-start; flex-direction: column; }
  }
  @media (max-height: 560px) and (min-width: 701px) { .sidebar-state { margin-top: auto; } }
  @media (max-width: 820px) {
    .recent-messages > .section-heading { gap: 8px; }
    .recent-messages > .section-heading h2 { white-space: nowrap; font-size: 15px; }
    .message-tools { gap: 4px; }
    .message-tools .filter-button { width: 32px; min-width: 32px; padding-inline: 4px; }
    .message-tools .text-button { font-size: 11px; }
  }
  .shell { grid-template-columns: 172px minmax(0, 1fr); }
  .sidebar { padding: 24px 8px 12px; background: var(--sidebar); }
  .brand-signature { min-height: 64px; margin: 0 10px 12px; gap: 5px; }
  .brand-wordmark { width: 90px; }
  .brand-name { font-size: 10px; }
  nav { gap: 2px; }
  nav button { min-height: 34px; padding-inline: 12px; font-size: 13px; }
  .sidebar-state { margin-top: auto; padding-top: 8px; font-size: 12px; }
  main { padding: 18px 18px 24px; font-size: 12px; container-type: inline-size; }
  main.status-page::before { display: none; }
  .status-content { gap: 0; }
  .status-line { min-height: 54px; padding: 0 8px 10px 12px; border-bottom: 1px solid var(--border); }
  .status-overview { gap: 0; }
  .status-heading { display: flex; align-items: flex-end; gap: 12px; }
  .status-line .status-heading h2 { margin: 0; white-space: nowrap; }
  .status-line .status-heading p { margin: 0; font-size: 12px; line-height: 1.2; white-space: nowrap; }
  .status-indicator { display: none; }
  .status-line h2, .product-list-heading h2 { font-size: 21px; line-height: 1.2; font-weight: 650; }
  .status-line p, .product-list-heading p { margin: 6px 0 0; font-size: 14px; }
  .status-line .primary { min-height: 34px; padding-inline: 12px; }
  .status-workspace { display: grid; grid-template-columns: minmax(0, 1fr) 8px clamp(220px, var(--messages-width), 60%); align-items: start; }
  .status-workspace.messages-collapsed { grid-template-columns: minmax(0, 1fr) 28px; }
  .status-workspace.resizing { cursor: col-resize; user-select: none; }
  .status-workspace > .group { border: 0; border-radius: 0; background: transparent; }
  .status-products { padding: 8px 8px 0 0; overflow: visible; }
  .recent-messages .section-heading h2 { font-size: 15px; }
  .product-image-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 190px), 1fr)); min-width: 0; gap: 8px 4px; }
  .message-resizer { align-self: stretch; cursor: col-resize; touch-action: none; position: relative; width: 8px; padding: 0; border: 0; border-radius: 0; background: transparent; }
  .message-resizer::before { content: ""; position: absolute; top: 8px; bottom: 0; left: 3px; width: 1px; background: var(--border); }
  .message-resizer:hover::before, .message-resizer:focus-visible::before, .resizing .message-resizer::before { width: 2px; background: var(--accent); }
  .message-resizer:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .recent-messages { min-width: 0; padding: 8px 0 0 8px; }
  .recent-messages > .section-heading { margin-bottom: 8px; }
  .recent-messages .section-heading { flex-wrap: wrap; gap: 4px; min-height: 24px; }
  .recent-messages .section-heading h2 { white-space: nowrap; }
  .recent-messages .message-tools { gap: 2px; margin-left: auto; flex-wrap: nowrap; }
  .recent-messages .filter-button, .recent-messages .icon-button { width: 24px; min-width: 24px; min-height: 24px; height: 24px; padding: 0; }
  .recent-messages .text-button { font-size: 10px; white-space: nowrap; padding: 2px 4px; }
  .message-filter-fields { display: grid; grid-template-columns: repeat(auto-fit, minmax(128px, 1fr)); gap: 6px; margin: 0 0 8px; }
  .message-filter-fields input[type="date"], .message-filter-fields select { width: 100%; min-width: 0; min-height: 26px; height: 26px; padding: 2px 6px; font-size: 10px; }
  .messages-collapsed .recent-messages { padding: 8px 0 0; }
  .messages-collapsed .recent-messages h2 { display: none; }
  .messages-collapsed .message-tools { margin: 0; }
  .recent-messages .message-scroll { max-height: 620px; border-top: 0; }
  .recent-messages .message-table { min-width: 0; font-size: 11px; }
  .recent-messages .message-table tr { grid-template-columns: 82px minmax(0, 1fr) minmax(0, 1.5fr); }
  .recent-messages .message-table td:nth-child(3), .recent-messages .message-table td:nth-child(4), .recent-messages .message-table th:nth-child(3), .recent-messages .message-table th:nth-child(4) { display: none; }
  .recent-messages .message-table td { padding: 6px 5px; }
  .recent-messages .message-table td:first-child { display: flex; flex-direction: column; justify-content: center; gap: 2px; padding-left: 0; color: var(--muted); }
  .message-day { font-size: 10px; line-height: 1.1; }
  .message-clock { font-size: 10px; line-height: 1.2; font-variant-numeric: tabular-nums; }
  .recent-messages .message-table td:last-child { text-align: right; }
  .product-content { display: block; }
  .product-content.drawer-open { margin-right: 394px; }
  .product-management { padding: 0; border: 0; border-radius: 0; background: transparent; }
  .product-list-heading { min-height: 64px; margin: 0; padding: 0 0 12px 4px; border-bottom: 1px solid var(--border); flex-wrap: wrap; }
  .product-list-heading p { color: var(--muted); }
  .product-list-heading .actions { gap: 12px; flex: none; }
  .product-management .product-toolbar { margin: 0; padding: 10px 4px; border: 0; border-bottom: 1px solid var(--border); border-radius: 0; background: transparent; }
  .product-detail-drawer { position: fixed; z-index: 9; top: 0; right: 0; bottom: 0; width: 394px; padding: 24px 18px 18px; border-left: 1px solid var(--border); overflow-y: auto; background: var(--surface); box-shadow: -8px 0 26px rgb(0 0 0 / 5%); }
  .drawer-heading { display: flex; align-items: center; justify-content: space-between; margin-bottom: 24px; }
  .drawer-heading h2 { margin: 0; font-size: 22px; }
  .drawer-photo { display: block; width: 100%; height: 180px; padding: 8px; border: 0; object-fit: contain; mix-blend-mode: var(--photo-blend); }
  .drawer-visual { border-radius: var(--photo-radius); background: var(--photo-surface); overflow: hidden; }
  .product-detail-drawer > h3 { margin: 14px 0 7px; font-size: 18px; line-height: 1.3; }
  .drawer-state { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 14px; color: var(--foreground); font-size: 12px; }
  .drawer-section { padding: 12px 0; border-top: 1px solid var(--border); }
  .drawer-section h4 { margin: 0 0 8px; font-size: 13px; }
  .drawer-section p { margin: 4px 0; color: var(--muted); font-size: 12px; }
  .drawer-records > div { display: flex; justify-content: space-between; gap: 12px; padding: 8px 0; border-bottom: 1px solid var(--hairline); font-size: 11px; }
  .drawer-records time { white-space: nowrap; color: var(--muted); }
  .prominent-test-row, .mode-setting-footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .prominent-test-row p { margin: 0; }
  .prominent-test-row button, .mode-setting-footer button { flex: none; }
  .prominent-toggle.enabled { color: var(--accent-text); background: var(--accent-soft); }
  .sidebar-state > span { margin-right: auto; }
  .github-link { color: var(--muted); text-decoration: none; }
  .github-link span { width: 15px; height: 15px; background: currentColor; mask: url("/brand/github.svg") center / contain no-repeat; }
  .status-aux { display: flex; flex-wrap: wrap; gap: 12px; margin-left: auto; color: var(--muted); }
  .status-aux .text-button { font-size: 11px; }
  .product-management-link { display: flex; justify-content: center; margin: 12px 0 0; }
  .product-management-link .text-button { color: var(--muted); font-size: 10px; }
  .product-choice-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 330px), 1fr)); gap: 8px; margin-top: 12px; }
  .choice-meta { display: flex; align-items: center; flex-wrap: wrap; gap: 3px 8px; color: var(--muted); font-size: 10px; }
  .choice-error { max-height: 100px; overflow: auto; margin: 4px 0 0; padding: 6px; border-radius: 4px; color: var(--danger); background: var(--danger-soft); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; font: 10px/1.5 ui-monospace, monospace; }
  .drawer-prominent { margin: 0 0 12px; }
  .prominent-test-row { margin-top: 14px; }
  .channel-heading { align-items: center; flex-wrap: wrap; }
  .monitoring-modes { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; margin: 0; padding: 0; border: 0; }
  .mode-choice { min-width: 0; padding: 14px; border: 1px solid var(--control-border); border-radius: 8px; background: var(--button-bg); cursor: pointer; }
  .mode-choice.chosen { border-color: var(--accent); background: var(--accent-soft); }
  .mode-choice-title { display: flex; align-items: baseline; gap: 8px; font-size: 13px; }
  .mode-choice input { accent-color: var(--accent); }
  .mode-choice p { color: var(--muted); font-size: 11px; line-height: 1.6; }
  .mode-choice h3 { margin: 12px 0 4px; font-size: 11px; }
  .mode-choice ul { margin: 0; padding-left: 17px; color: var(--muted); font-size: 11px; line-height: 1.8; }
  .mode-setting-footer { margin-top: 16px; justify-content: flex-end; }
  .preview-alert { position: fixed; z-index: 20; inset: 0; }
  .preview-alert iframe { width: 100%; height: 100%; border: 0; background: transparent; }
  .preview-alert-close { position: absolute; top: 10px; right: 10px; padding: 6px; border: 0; background: transparent; color: #fff; }
  @container (max-width: 520px) { .monitoring-modes { grid-template-columns: 1fr; } }
  @media (max-width: 1180px) { .product-content.drawer-open { margin-right: 0; } .product-detail-drawer { box-shadow: -12px 0 35px rgb(0 0 0 / 18%); } }
  @media (max-width: 900px) { .shell { grid-template-columns: 164px minmax(0, 1fr); } .sidebar { padding-inline: 10px; } .brand-wordmark { width: 84px; } .brand-name { font-size: 10px; } nav button { font-size: 13px; } .product-list-heading { flex-wrap: wrap; gap: 12px; } .product-list-heading .actions { width: 100%; } }
  @container (max-width: 600px) { .status-workspace, .status-workspace.messages-collapsed { grid-template-columns: 1fr; } .message-resizer { display: none; } .recent-messages { border-top: 1px solid var(--border) !important; padding: 10px 0; } .messages-collapsed .recent-messages { padding: 8px 0; } .messages-collapsed .recent-messages h2 { display: block; } .messages-collapsed .message-tools { margin-left: auto; } }
  @media (max-width: 700px) { .shell { display: block; } .sidebar { padding: 10px 12px; } .brand-signature { min-height: 44px; margin: 0 8px 8px; flex-direction: row; } .brand-wordmark { width: 54px; } nav button { min-height: 34px; } main { padding: 20px 14px 32px; } .status-line { flex-wrap: wrap; } .product-detail-drawer { width: min(100vw, 394px); } .prominent-test-row { flex-wrap: wrap; } }
  @media (max-width: 620px) { .product-management .product-toolbar { flex-wrap: wrap; } }
  @media (prefers-reduced-motion: reduce) { nav button, button.primary, button.secondary, button.danger-button { transition: none; } }
</style>
