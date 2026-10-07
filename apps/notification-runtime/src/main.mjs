import readline from 'node:readline';
import { registerApp, Client as FeishuClient, WSClient as FeishuWSClient, EventDispatcher as FeishuEventDispatcher } from '@larksuiteoapi/node-sdk';
import { WSClient as WecomWSClient } from '@wecom/aibot-node-sdk';
import { createWeixinTextApi } from './weixin-text-api.mjs';
import { pathToFileURL } from 'node:url';
import axios from 'axios';
import HttpsProxyAgent from 'https-proxy-agent';
import { AsyncLocalStorage } from 'node:async_hooks';
import { isDeepStrictEqual } from 'node:util';
import { EventEmitter, once } from 'node:events';

for (const method of ['log', 'info', 'warn', 'error', 'debug']) console[method] = () => {};

const write = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const safeError = (error) => error?.name === 'AbortError' ? '已取消' : '平台请求失败，请稍后重试';
const numericCode = (value) => /^-?\d+$/.test(String(value)) ? String(value) : undefined;
const connectionCredentials = (provider, c) => provider === 'weixin'
  ? { token: c.botToken ?? c.token, baseUrl: c.baseUrl ?? 'https://ilinkai.weixin.qq.com/', accountId: c.accountId, userId: c.userId }
  : provider === 'wecom' ? { botId: c.botId, secret: c.secret } : { appId: c.appId, appSecret: c.appSecret };
const sleep = (ms, signal) => new Promise((resolve, reject) => {
  const timer = setTimeout(resolve, ms);
  signal.addEventListener('abort', () => { clearTimeout(timer); reject(signal.reason); }, { once: true });
});

export class Runtime {
  constructor({ fetchImpl = globalThis.fetch, register = registerApp, emit = write, sleepImpl = sleep, sdk = {} } = {}) {
    this.baseFetch = fetchImpl;
    this.originalGlobalFetch = globalThis.fetch;
    this.originalAxiosDefaults = { proxy: axios.defaults.proxy, httpAgent: axios.defaults.httpAgent, httpsAgent: axios.defaults.httpsAgent };
    this.requestStorage = new AsyncLocalStorage();
    this.fetch = (input, options = {}) => {
      const requestSignal = this.requestStorage.getStore();
      const signal = requestSignal && options.signal ? AbortSignal.any([requestSignal, options.signal]) : requestSignal ?? options.signal;
      return fetchImpl(input, {
        ...options,
        ...(signal ? { signal } : {}),
        ...(globalThis.Bun ? { proxy: this.proxyUrl ?? '' } : {}),
      });
    };
    this.axiosInterceptorId = axios.interceptors.request.use((config) => this.applyRequestSignal(config));
    this.register = register;
    this.emit = emit;
    this.sleep = sleepImpl;
    this.sdk = {
      FeishuClient: sdk.FeishuClient ?? FeishuClient,
      FeishuWSClient: sdk.FeishuWSClient ?? FeishuWSClient,
      FeishuEventDispatcher: sdk.FeishuEventDispatcher ?? FeishuEventDispatcher,
      WecomWSClient: sdk.WecomWSClient ?? WecomWSClient,
      DWClient: sdk.DWClient,
    };
    this.weixinApi = createWeixinTextApi(this.fetch);
  }
  accounts = new Map();
  bindings = new Map();
  network = 'direct';
  proxyUrl;
  closing = false;
  inFlight = new Set();
  accountChanges = new EventEmitter().setMaxListeners(0);

  applyRequestSignal(config) {
    const scope = this.requestStorage.getStore();
    const signal = scope && config.timeout ? AbortSignal.any([scope, AbortSignal.timeout(config.timeout)]) : scope;
    if (signal) config.signal = config.signal ? AbortSignal.any([signal, config.signal]) : signal;
    if (signal && this.proxyAgent) {
      const agent = new HttpsProxyAgent.HttpsProxyAgent({ ...this.proxyAgent.proxy, signal: config.signal });
      config.httpAgent = agent;
      config.httpsAgent = agent;
    }
    return config;
  }

  accountProxyAgent(account) {
    return this.proxyAgent ? new HttpsProxyAgent.HttpsProxyAgent({ ...this.proxyAgent.proxy, signal: account.abort.signal }) : undefined;
  }

  event(event, data) {
    if (event === 'account') this.accountChanges.emit(`account:${data.id}`);
    this.emit({ event, data });
  }

  async call(method, params = {}) {
    if (method === 'configure') return this.configure(params);
    if (method === 'begin_binding') return this.beginBinding(params);
    if (method === 'binding_status') return this.bindingStatus(params.bindingId);
    if (method === 'cancel_binding') return this.cancelBinding(params.bindingId);
    if (method === 'detect_groups') return this.detectGroups(params.accountId);
    if (method === 'detect_binding_groups') return this.detectBindingGroups(params.bindingId);
    if (method === 'status') return this.status();
    if (method === 'send') return this.send(params);
    if (method === 'shutdown') { await this.close(); return { stopped: true }; }
    throw Object.assign(new Error('未知方法'), { code: -32601 });
  }

  configure({ accounts = [], network = 'direct', proxyUrl } = {}) {
    if (!['direct', 'system_proxy'].includes(network)) throw new TypeError('network 无效');
    if (network === 'system_proxy' && !proxyUrl) throw new TypeError('系统代理地址未提供');
    this.network = network;
    this.proxyUrl = network === 'system_proxy' ? proxyUrl : undefined;
    globalThis.fetch = this.fetch;
    this.proxyAgent = this.proxyUrl ? new HttpsProxyAgent.HttpsProxyAgent(this.proxyUrl) : undefined;
    this.httpInstance = axios.create(this.proxyUrl
      ? { proxy: false, timeout: 15_000, httpAgent: this.proxyAgent, httpsAgent: this.proxyAgent }
      : { proxy: false, timeout: 15_000 });
    this.httpInstance.interceptors.request.use((config) => this.applyRequestSignal(config));
    this.httpInstance.interceptors.response.use((response) => response.config.$return_headers
      ? { data: response.data, headers: response.headers }
      : response.data);
    axios.defaults.proxy = false;
    axios.defaults.httpAgent = this.proxyAgent;
    axios.defaults.httpsAgent = this.proxyAgent;
    const next = new Map();
    for (const account of accounts) {
      if (!account.id || !account.provider || !account.credentials || !Array.isArray(account.targets)) throw new TypeError('账号配置不完整');
      if (!['feishu', 'wecom', 'dingtalk', 'weixin'].includes(account.provider)) throw new TypeError('通知渠道不受支持');
      next.set(account.id, { ...account, transportKey: this.proxyUrl ?? 'direct', status: account.enabled ? 'connecting' : 'stopped' });
    }
    for (const [id, old] of this.accounts) {
      const replacement = next.get(id);
      if (old.provisional && [...next.values()].some((account) => account.provider === old.provider && isDeepStrictEqual(connectionCredentials(account.provider, account.credentials), connectionCredentials(old.provider, old.credentials)))) {
        this.stopAccount(old);
        continue;
      }
      if (!replacement && old.provisional) { next.set(id, old); continue; }
      if (replacement && replacement.provider === old.provider && isDeepStrictEqual(connectionCredentials(replacement.provider, replacement.credentials), connectionCredentials(old.provider, old.credentials)) && replacement.transportKey === old.transportKey && replacement.enabled) {
        const { status, targets, credentials } = old;
        Object.assign(old, replacement, { status, targets, credentials, provisional: false });
        for (const target of replacement.targets) this.discoverTarget(old, target);
        next.set(id, old);
      } else {
        this.stopAccount(old);
      }
    }
    this.accounts = next;
    for (const account of this.accounts.values()) {
      if (!account.client && !account.ws && !account.abort) account.status = account.enabled ? 'connecting' : 'stopped';
      this.event('account', this.publicAccount(account));
      if (account.enabled) void this.startAccount(account);
    }
    return { configured: this.accounts.size };
  }

  async startAccount(account) {
    if (account.client || account.ws || account.abort) return;
    try {
      if (!this.isCurrent(account)) return;
      if (account.provider === 'feishu') await this.startFeishu(account);
      else if (account.provider === 'wecom') await this.startWecom(account);
      else if (account.provider === 'dingtalk') await this.startDingtalk(account);
      else if (account.provider === 'weixin') await this.startWeixin(account);
      else account.status = 'auth_required';
      if (!this.isCurrent(account)) { this.stopAccount(account); return; }
      if (!account.supervisor && ['wecom', 'dingtalk'].includes(account.provider)) this.startSupervisor(account);
    } catch {
      if (!this.isCurrent(account)) return;
      account.status = 'failed';
      account.retryAt = Date.now() + 30_000;
      account.message = '连接失败，请检查账号授权或网络';
      if (['wecom', 'dingtalk'].includes(account.provider) && !account.supervisor) this.startSupervisor(account);
      process.stderr.write(`[notification-runtime] ${account.provider} connection failed\n`);
    }
    this.event('account', this.publicAccount(account));
  }

  isCurrent(account) { return !this.closing && account.enabled && this.accounts.get(account.id) === account; }
  ownsTransport(account, transport, property) { return this.isCurrent(account) && account[property] === transport; }

  isAuthError(error) {
    return error?.name === 'WSAuthFailureError'
      || [401, 403].includes(error?.status ?? error?.statusCode);
  }

  stopAccount(account) {
    clearInterval(account.supervisor);
    account.supervisor = undefined;
    account.abort?.abort();
    for (const controller of account.sendControllers ?? []) controller.abort();
    const client = account.client;
    try { account.ws?.close?.({ force: true }); } catch {}
    try { client?.stop?.(); } catch {}
    try { client?.disconnect?.(); } catch {}
    if (client && account.connectTask && !account.connectSettled) {
      void account.connectTask.then(() => client.disconnect()).catch(() => undefined);
    }
    account.connectTask = undefined;
    account.connectSettled = true;
    account.ws = undefined;
    account.client = undefined;
  }

  startSupervisor(account) {
    account.supervisor = setInterval(() => this.syncAccountStatus(account), account.provider === 'dingtalk' ? 1_000 : 15_000);
    account.supervisor.unref?.();
  }

  syncAccountStatus(account) {
    if (!this.isCurrent(account)) return;
    const previousStatus = account.status;
    if (account.provider === 'wecom' && account.client?.isConnected) account.status = 'ready';
    if (account.provider === 'dingtalk') {
      if (account.status === 'auth_required') return;
      if (account.client?.connected) { account.status = 'ready'; account.lastReadyAt = Date.now(); account.disconnectedAt = undefined; }
      else if (account.status === 'ready' || !account.disconnectedAt) { account.status = 'reconnecting'; account.disconnectedAt = Date.now(); }
      if (!account.client?.connected && account.disconnectedAt && Date.now() - account.disconnectedAt > 45_000) {
        account.retryAt ??= Date.now() + 30_000;
        account.status = 'failed';
      }
    }
    if (account.status === 'failed' && account.retryAt && Date.now() >= account.retryAt) void this.restartAccount(account);
    if (account.status !== previousStatus) this.event('account', this.publicAccount(account));
  }

  async restartAccount(account) {
    if (!this.isCurrent(account) || account.restarting || account.status === 'auth_required') return;
    account.restarting = true;
    account.retryAt = Date.now() + 30_000;
    this.stopAccount(account);
    account.status = 'reconnecting';
    try {
      if (account.provider === 'wecom') await this.startWecom(account);
      if (account.provider === 'dingtalk') await this.startDingtalk(account);
    } catch { account.status = 'failed'; }
    account.restarting = false;
    if (!account.supervisor && this.isCurrent(account)) this.startSupervisor(account);
    this.event('account', this.publicAccount(account));
  }

  async startFeishu(account) {
    const { appId, appSecret } = account.credentials;
    if (!appId || !appSecret) { account.status = 'auth_required'; return; }
    account.abort = new AbortController();
    account.client = new this.sdk.FeishuClient({ appId, appSecret, httpInstance: this.httpInstance, source: 'ricoh-monitor' });
    if (account.client.request) void this.requestStorage.run(account.abort.signal, () => account.client.request({ url: '/open-apis/bot/v3/info', method: 'GET' })).then((response) => {
      const name = response?.bot?.app_name;
      if (!this.isCurrent(account) || !name || name === account.credentials.botName) return;
      account.credentials.botName = name;
      this.event('credentials', { accountId: account.id, credentials: { botName: name } });
      if (account.provisionalBinding) this.event('binding', this.publicBinding(account.provisionalBinding));
    }).catch(() => {});
    let ws;
    ws = new this.sdk.FeishuWSClient({
      appId, appSecret, source: 'ricoh-monitor', autoReconnect: true,
      agent: this.accountProxyAgent(account), httpInstance: this.httpInstance,
      logger: { debug() {}, info() {}, warn() {}, error() {} },
      onReady: () => { if (!this.ownsTransport(account, ws, 'ws')) return; account.status = 'ready'; this.event('account', this.publicAccount(account)); },
      onReconnecting: () => { if (!this.ownsTransport(account, ws, 'ws')) return; account.status = 'reconnecting'; this.event('account', this.publicAccount(account)); },
      onReconnected: () => { if (!this.ownsTransport(account, ws, 'ws')) return; account.status = 'ready'; this.event('account', this.publicAccount(account)); },
      onError: (error) => {
        if (!this.ownsTransport(account, ws, 'ws')) return;
        account.status = this.isAuthError(error) ? 'auth_required' : 'failed';
        if (account.status === 'failed') account.retryAt = Date.now() + 30_000;
        this.event('account', this.publicAccount(account));
      },
      handshakeTimeoutMs: 15_000,
    });
    account.ws = ws;
    account.dispatcher = new this.sdk.FeishuEventDispatcher({}).register({
      'im.message.receive_v1': (event) => {
        const message = event?.message;
        const target = message?.chat_type === 'group'
          ? { id: message.chat_id, kind: 'chat', label: message.chat_id }
          : { id: event?.sender?.sender_id?.open_id, kind: 'user', label: event?.sender?.sender_id?.open_id };
        this.discoverTarget(account, target);
      },
    });
    this.requestStorage.run(account.abort.signal, () => account.ws.start({ eventDispatcher: account.dispatcher }));
    account.supervisor = setInterval(() => {
      if (!this.ownsTransport(account, ws, 'ws')) return;
      const state = account.ws.getConnectionStatus?.().state;
      if (state === 'reconnecting' || state === 'connecting') account.status = 'reconnecting';
      else if (state === 'connected') account.status = 'ready';
      else if (state === 'failed' && Date.now() >= (account.retryAt ?? Infinity)) {
        account.retryAt = Date.now() + 30_000;
        account.status = 'reconnecting';
        this.requestStorage.run(account.abort.signal, () => account.ws.start({ eventDispatcher: account.dispatcher }));
      }
      this.event('account', this.publicAccount(account));
    }, 15_000);
    account.supervisor.unref?.();
  }

  async startWecom(account) {
    const { botId, secret } = account.credentials;
    if (!botId || !secret) { account.status = 'auth_required'; return; }
    account.abort = new AbortController();
    const client = new this.sdk.WecomWSClient({ botId, secret, requestTimeout: 10_000, wsOptions: this.proxyAgent ? { agent: this.accountProxyAgent(account) } : {}, logger: { debug() {}, info() {}, warn() {}, error() {} } });
    account.client = client;
    client.on('authenticated', () => { if (!this.ownsTransport(account, client, 'client')) return; account.status = 'ready'; this.event('account', this.publicAccount(account)); });
    client.on('error', (error) => {
      if (!this.ownsTransport(account, client, 'client')) return;
      if (this.isAuthError(error)) account.status = 'auth_required';
      else if (error?.name === 'WSReconnectExhaustedError') { account.status = 'failed'; account.retryAt = Date.now() + 30_000; }
      else account.status = 'reconnecting';
      this.event('account', this.publicAccount(account));
    });
    client.on('reconnecting', () => { if (!this.ownsTransport(account, client, 'client')) return; account.status = 'reconnecting'; this.event('account', this.publicAccount(account)); });
    client.on('disconnected', () => { if (this.ownsTransport(account, client, 'client')) { account.status = 'reconnecting'; this.event('account', this.publicAccount(account)); } });
    client.on('close', () => { if (this.ownsTransport(account, client, 'client')) { account.status = 'reconnecting'; this.event('account', this.publicAccount(account)); } });
    client.on('message', (frame) => {
      if (!this.ownsTransport(account, client, 'client')) return;
      const body = frame?.body ?? {};
      const target = body.chatid
        ? { id: body.chatid, kind: 'chat', label: body.chatname ?? body.chatid }
        : { id: body.from?.userid, kind: 'user', label: body.from?.name ?? body.from?.userid };
      this.discoverTarget(account, target);
    });
    client.connect();
    account.startedAt = Date.now();
  }

  discoverTarget(account, target) {
    if (!this.isCurrent(account) || !target?.id) return;
    const existing = account.targets.find((item) => item.id === target.id && item.kind === target.kind);
    if (existing) {
      if (!target.label || target.label === target.id) target = { ...target, label: existing.label || target.label };
      Object.assign(existing, target);
    } else account.targets.push(target);
    if (account.provisionalBinding) {
      account.provisionalBinding.targets = account.targets;
      this.event('binding', this.publicBinding(account.provisionalBinding));
    }
    this.event('target', { accountId: account.id, target });
    this.event('account', this.publicAccount(account));
  }

  async detectGroups(accountId) {
    const account = this.accounts.get(accountId);
    if (!account || account.status !== 'ready') throw Object.assign(new Error('账号未就绪'), { code: -32004 });
    if (!['feishu', 'wecom', 'dingtalk'].includes(account.provider)) throw Object.assign(new Error('该渠道不支持群组检测'), { code: -32004 });
    const targets = account.provider === 'feishu'
      ? await this.listFeishuGroups(account)
      : account.targets.filter((target) => target.kind === 'chat');
    for (const target of targets) this.discoverTarget(account, target);
    return { targets };
  }

  async detectBindingGroups(bindingId) {
    const binding = this.bindings.get(bindingId);
    const account = this.accounts.get(bindingId);
    if (!binding || !['feishu', 'wecom', 'dingtalk'].includes(binding.provider) || binding.status !== 'complete' || !account || account.provider !== binding.provider) throw Object.assign(new Error('绑定尚未完成或不支持群组检测'), { code: -32004 });
    const { targets } = await this.detectGroups(bindingId);
    binding.targets = [...account.targets];
    this.event('binding', this.publicBinding(binding));
    return { targets };
  }

  async listFeishuGroups(account) {
    let pageToken;
    const targets = [];
    do {
      const response = await this.requestStorage.run(account.abort.signal, () => account.client.im.v1.chat.list({ params: { page_size: 100, ...(pageToken ? { page_token: pageToken } : {}) } }));
      if (response?.code !== 0) throw Object.assign(new Error('群列表读取失败'), { code: response?.code });
      for (const chat of response.data?.items ?? []) {
        if (chat.chat_id && chat.chat_mode !== 'p2p') targets.push({ id: chat.chat_id, kind: 'chat', label: chat.name || chat.chat_id });
      }
      if (response.data?.has_more && !response.data?.page_token) throw new Error('群列表分页响应不完整');
      pageToken = response.data?.has_more ? response.data.page_token : undefined;
    } while (pageToken);
    return targets;
  }

  async startProvisionalBinding(binding) {
    if (this.closing || binding.controller.signal.aborted || this.bindings.get(binding.id) !== binding) return;
    const account = { id: binding.id, provider: binding.provider, credentials: binding.credentials, enabled: true, targets: binding.targets, status: 'connecting', provisional: true, provisionalBinding: binding };
    this.accounts.set(account.id, account);
    this.event('account', this.publicAccount(account));
    await this.startAccount(account);
  }

  async startDingtalk(account) {
    const { appId, appSecret } = account.credentials;
    if (!appId || !appSecret) { account.status = 'auth_required'; return; }
    const dingtalk = this.sdk.DWClient ? { DWClient: this.sdk.DWClient, TOPIC_ROBOT: 'CALLBACK:robot' } : await import('dingtalk-stream');
    const { DWClient, TOPIC_ROBOT } = dingtalk;
    if (!this.isCurrent(account)) return;
    account.abort = new AbortController();
    const client = new DWClient({ clientId: appId, clientSecret: appSecret, endpoint: 'https://api.dingtalk.com', autoReconnect: false, keepAlive: true, debug: false });
    client.sslopts = { ...client.sslopts, ...(this.proxyAgent ? { agent: this.accountProxyAgent(account) } : {}) };
    account.client = client;
    client.registerCallbackListener(TOPIC_ROBOT, (response) => {
      if (!this.ownsTransport(account, client, 'client')) return;
      const messageId = response?.headers?.messageId;
      if (messageId) client.socketCallBackResponse(messageId, { success: true });
      try {
        const message = typeof response?.data === 'string' ? JSON.parse(response.data) : response?.data;
        const group = String(message?.conversationType) === '2';
        if (!group && message?.senderStaffId && message?.senderId && message.senderId !== message.senderStaffId
          && account.targets.some((target) => target.kind === 'user' && target.id === message.senderId)) {
          // senderId 与 senderStaffId 是平台提供的同一身份，投递使用员工 ID。
          const targetAliases = { ...account.credentials.targetAliases, [message.senderId]: message.senderStaffId };
          account.credentials.targetAliases = targetAliases;
          account.targets = [...new Map(account.targets.map((target) => {
            const normalized = target.kind === 'user' && targetAliases[target.id] ? { ...target, id: targetAliases[target.id] } : target;
            return [`${normalized.kind}:${normalized.id}`, normalized];
          })).values()];
          this.event('credentials', { accountId: account.id, credentials: { targetAliases } });
        }
        this.discoverTarget(account, group
          ? { id: message?.conversationId, kind: 'chat', label: message?.conversationTitle ?? message?.conversationId }
          : { id: message?.senderStaffId, kind: 'user', label: message?.senderNick ?? message?.senderStaffId });
      } catch { /* malformed platform callbacks do not affect the stream */ }
    });
    client.on('connected', () => { if (!this.ownsTransport(account, client, 'client')) return; account.status = 'ready'; account.lastReadyAt = Date.now(); account.disconnectedAt = undefined; this.event('account', this.publicAccount(account)); });
    client.on('disconnected', () => { if (this.ownsTransport(account, client, 'client')) { account.status = 'reconnecting'; this.event('account', this.publicAccount(account)); } });
    account.connectSettled = false;
    const signal = AbortSignal.any([account.abort.signal, AbortSignal.timeout(15_000)]);
    const connectTask = this.requestStorage.run(signal, () => Promise.resolve().then(() => client.connect()));
    account.connectTask = connectTask;
    this.inFlight.add(connectTask);
    try { await connectTask; }
    finally {
      this.inFlight.delete(connectTask);
      if (account.connectTask === connectTask) account.connectSettled = true;
    }
    if (!this.ownsTransport(account, client, 'client')) { client.disconnect(); return; }
    account.status = client.connected ? 'ready' : 'connecting';
    account.startedAt = Date.now();
    account.disconnectedAt = undefined;
  }

  async startWeixin(account) {
    const c = account.credentials;
    account.abort = new AbortController();
    c.token = c.botToken ?? c.token;
    c.baseUrl ??= 'https://ilinkai.weixin.qq.com/';
    c.contextTokens ??= {};
    if (!c.token) { account.status = 'auth_required'; return; }
    account.status = 'ready';
    this.event('account', this.publicAccount(account));
    void this.pollWeixin(account);
  }

  async pollWeixin(account) {
    while (!account.abort.signal.aborted) {
      try {
        const updates = await this.weixinApi.getUpdates({ baseUrl: account.credentials.baseUrl, token: account.credentials.token, cursor: account.credentials.getUpdatesBuf ?? '', signal: account.abort.signal });
        const providerCode = [updates?.errcode, updates?.ret].find((value) => value !== undefined && value !== 0 && value !== '0');
        if (providerCode !== undefined) {
          if (String(providerCode) === '-14') {
            account.status = 'auth_required';
            this.event('account', this.publicAccount(account));
            return;
          }
          account.status = 'reconnecting';
          this.event('account', this.publicAccount(account));
          await this.sleep(2_000, account.abort.signal);
          continue;
        }
        const credentials = {};
        if (updates.get_updates_buf && updates.get_updates_buf !== account.credentials.getUpdatesBuf) credentials.getUpdatesBuf = updates.get_updates_buf;
        account.credentials.getUpdatesBuf = updates.get_updates_buf ?? account.credentials.getUpdatesBuf ?? '';
        for (const message of updates.msgs ?? []) {
          const userId = message.from_user_id;
          if (!userId) continue;
          this.discoverTarget(account, { id: userId, kind: 'user', label: userId });
          if (message.context_token && account.credentials.contextTokens[userId] !== message.context_token) {
            account.credentials.contextTokens[userId] = message.context_token;
            credentials.contextTokens = { ...account.credentials.contextTokens };
          }
        }
        if (Object.keys(credentials).length) this.event('credentials', { accountId: account.id, credentials });
        account.status = 'ready';
      } catch (error) {
        if (account.abort.signal.aborted) return;
        if ([401, 403].includes(error?.status)) {
          account.status = 'auth_required';
          this.event('account', this.publicAccount(account));
          return;
        }
        account.status = 'reconnecting';
        this.event('account', this.publicAccount(account));
        await this.sleep(2_000, account.abort.signal);
      }
    }
  }

  async beginBinding({ provider, bindingId, appId }) {
    if (!bindingId || !['feishu', 'wecom', 'dingtalk', 'weixin'].includes(provider)) throw new TypeError('不支持该扫码渠道');
    this.cancelBinding(bindingId);
    const attempt = { id: bindingId, provider, status: 'waiting', controller: new AbortController(), targets: [], ...(appId ? { appId } : {}) };
    this.bindings.set(bindingId, attempt);
    this.event('binding', this.publicBinding(attempt));
    void this.runBinding(attempt).catch((error) => {
      if (attempt.status === 'cancelled') return;
      attempt.status = 'failed';
      attempt.message = safeError(error);
      this.event('binding', this.publicBinding(attempt));
    });
    return this.publicBinding(attempt);
  }

  async runBinding(a) {
    if (a.provider === 'feishu') return this.bindFeishu(a);
    if (a.provider === 'wecom') return this.bindWecom(a);
    if (a.provider === 'weixin') return this.bindWeixin(a);
    return this.bindDingtalk(a);
  }

  async bindWeixin(a) {
    const login = await this.weixinApi.beginLogin({ signal: a.controller.signal });
    a.code = login.qrcode;
    a.qrUrl = login.qrUrl;
    this.event('binding', this.publicBinding(a));
    const deadline = Date.now() + 5 * 60_000;
    let baseUrl = 'https://ilinkai.weixin.qq.com/';
    while (!a.controller.signal.aborted && Date.now() < deadline) {
      const result = await this.weixinApi.pollLogin({ qrcode: a.code, baseUrl, signal: a.controller.signal });
      if (result.status === 'scaned' || result.status === 'scaned_but_redirect') a.status = 'scanned';
      if (result.status === 'scaned_but_redirect') baseUrl = result.redirect_host;
      if (result.status === 'confirmed') {
        if (!result.bot_token || !result.ilink_bot_id || !result.ilink_user_id) throw Error('login credentials missing');
        a.credentials = { botToken: result.bot_token, accountId: result.ilink_bot_id, userId: result.ilink_user_id, baseUrl: result.baseurl ?? baseUrl, contextTokens: {} };
        a.targets = [{ id: result.ilink_user_id, kind: 'user', label: '绑定账号' }];
        a.status = 'complete';
        void this.startProvisionalBinding(a);
        break;
      }
      if (result.status === 'expired') { a.status = 'expired'; break; }
      this.event('binding', this.publicBinding(a));
    }
    if (a.status !== 'complete' && Date.now() >= deadline) a.status = 'expired';
    this.event('binding', this.publicBinding(a));
  }

  async bindFeishu(a) {
    const registration = this.register({
      ...(a.appId ? { appId: a.appId } : { appPreset: { name: '理光库存监控' } }),
      createOnly: !a.appId,
      source: 'ricoh-monitor',
      addons: {
        scopes: { tenant: ['im:chat:read', 'im:message:send_as_bot', 'im:message.p2p_msg:readonly', 'im:message.group_at_msg:readonly'] },
        events: { items: { tenant: ['im.message.receive_v1'] } },
      },
      signal: a.controller.signal,
      onQRCodeReady: ({ url }) => {
        a.qrUrl = url;
        this.event('binding', this.publicBinding(a));
      },
    });
    const result = await registration;
    if (a.status === 'cancelled' || a.controller.signal.aborted || this.closing) return;
    a.credentials = { appId: result.client_id, appSecret: result.client_secret, ...(result.user_info?.open_id ? { userOpenId: result.user_info.open_id } : {}) };
    a.status = 'complete';
    a.targets = result.user_info?.open_id ? [{ id: result.user_info.open_id, kind: 'user', label: '绑定账号' }] : [];
    void this.startProvisionalBinding(a);
    this.event('binding', this.publicBinding(a));
  }

  async bindWecom(a) {
    const q = new URL('https://work.weixin.qq.com/ai/qc/generate');
    q.searchParams.set('source', 'deepseek-harness');
    q.searchParams.set('plat', '1');
    const data = await (await this.fetch(q, { signal: a.controller.signal })).json();
    a.code = data?.data?.scode;
    a.qrUrl = data?.data?.auth_url;
    if (!a.code || !a.qrUrl) throw Error('invalid registration response');
    this.event('binding', this.publicBinding(a));
    const deadline = Date.now() + 5 * 60_000;
    while (!a.controller.signal.aborted && Date.now() < deadline) {
      await this.sleep(3000, a.controller.signal);
      const url = new URL('https://work.weixin.qq.com/ai/qc/query_result');
      url.searchParams.set('scode', a.code);
      const result = await (await this.fetch(url, { signal: a.controller.signal })).json();
      const state = result?.data?.status;
      if (state === 'scaned') a.status = 'scanned';
      if (state === 'success') {
        a.credentials = { botId: result.data.bot_info.botid, secret: result.data.bot_info.secret, ...(result.data.bot_info.bot_name ? { botName: result.data.bot_info.bot_name } : {}) };
        a.status = 'complete';
        a.targets = [];
        void this.startProvisionalBinding(a);
        break;
      }
      if (['expired', 'timeout'].includes(state)) { a.status = 'expired'; break; }
      if (['fail', 'failed', 'error'].includes(state)) { a.status = 'failed'; break; }
    }
    if (a.status === 'waiting' && Date.now() >= deadline) a.status = 'expired';
    this.event('binding', this.publicBinding(a));
  }

  async bindDingtalk(a) {
    const post = async (path, body) => {
      const response = await this.fetch(`https://oapi.dingtalk.com${path}`, {
        method: 'POST', headers: { accept: 'application/json', 'content-type': 'application/json' },
        body: JSON.stringify(body), signal: a.controller.signal,
      });
      const value = await response.json();
      if (!response.ok || Number(value.errcode) !== 0) throw Error('registration request failed');
      return value;
    };
    const init = await post('/app/registration/init', { source: 'DING_DWS_CLAW' });
    const begun = await post('/app/registration/begin', { nonce: init.nonce });
    a.code = begun.device_code;
    a.qrUrl = begun.verification_uri_complete;
    if (!a.code || !a.qrUrl) throw Error('invalid registration response');
    this.event('binding', this.publicBinding(a));
    const deadline = Date.now() + Number(begun.expires_in || 7200) * 1000;
    while (!a.controller.signal.aborted && Date.now() < deadline) {
      await this.sleep(Number(begun.interval || 5) * 1000, a.controller.signal);
    const result = await post('/app/registration/poll', { device_code: a.code });
      if (result.status === 'SUCCESS') {
        a.credentials = { appId: result.client_id, appSecret: result.client_secret, ...(result.bot_name ? { botName: result.bot_name } : {}) };
        a.status = 'complete';
        a.targets = [];
        void this.startProvisionalBinding(a);
        break;
      }
      if (result.status === 'EXPIRED') { a.status = 'expired'; break; }
      if (result.status === 'FAIL') { a.status = 'failed'; break; }
    }
    if (a.status === 'waiting' && Date.now() >= deadline) a.status = 'expired';
    this.event('binding', this.publicBinding(a));
  }

  publicBinding(a) {
    return { id: a.id, provider: a.provider, status: a.status, ...(a.qrUrl ? { qrUrl: a.qrUrl } : {}), ...(a.message ? { message: a.message } : {}), ...(a.status === 'complete' ? { targets: a.targets, ...(a.credentials?.botName ? { botName: a.credentials.botName } : {}) } : {}) };
  }

  bindingStatus(id) {
    const a = this.bindings.get(id);
    if (!a) throw Object.assign(new Error('绑定任务不存在'), { code: -32004 });
    return { ...this.publicBinding(a), ...(a.status === 'complete' ? { credentials: a.credentials } : {}) };
  }

  cancelBinding(id) {
    const a = this.bindings.get(id);
    if (!a || ['expired', 'cancelled', 'failed'].includes(a.status)) return { cancelled: false };
    const provisional = this.accounts.get(id);
    if (a.status === 'complete' && !provisional?.provisional) return { cancelled: false };
    a.status = 'cancelled';
    a.controller.abort();
    if (provisional?.provisional) {
      this.stopAccount(provisional);
      this.accounts.delete(id);
    }
    this.event('binding', this.publicBinding(a));
    return { cancelled: true };
  }

  publicAccount(a) { return { id: a.id, status: a.status, targets: a.targets }; }
  status() { return { accounts: [...this.accounts.values()].map((a) => this.publicAccount(a)) }; }

  async send(params) {
    const account = this.accounts.get(params.accountId);
    if (!account) return { outcome: 'failed', message: '账号未连接', retryable: false };
    const controller = new AbortController();
    account.sendControllers ??= new Set();
    account.sendControllers.add(controller);
    const timeout = setTimeout(() => controller.abort(new DOMException('发送超时', 'TimeoutError')), 20_000);
    const task = this.performSend(params, controller.signal);
    this.inFlight.add(task);
    try { return await task; }
    finally { clearTimeout(timeout); account.sendControllers.delete(controller); this.inFlight.delete(task); }
  }

  async performSend({ accountId, target, text }, signal) {
    const account = this.accounts.get(accountId);
    if (!account) return { outcome: 'failed', message: '账号未连接', retryable: false };
    const readyUntil = Date.now() + 15_000;
    while (['connecting', 'reconnecting'].includes(account.status) && Date.now() < readyUntil && !signal.aborted) {
      try { await once(this.accountChanges, `account:${account.id}`, { signal: AbortSignal.any([signal, AbortSignal.timeout(readyUntil - Date.now())]) }); }
      catch { break; }
    }
    if (signal.aborted) return { outcome: 'unknown', message: '平台未确认发送结果' };
    if (account.status !== 'ready') return { outcome: 'failed', message: account.status === 'auth_required' ? '账号需要重新授权' : account.status === 'failed' ? '账号连接失败' : '账号连接超时', retryable: ['connecting', 'reconnecting'].includes(account.status) };
    if (!target?.id || typeof text !== 'string' || !text) return { outcome: 'failed', message: '发送参数无效', retryable: false };
    try {
      if (signal.aborted) return { outcome: 'unknown', message: '平台未确认发送结果' };
      const delivery = () => account.provider === 'weixin' ? this.sendWeixin(account, target, text, signal)
        : account.provider === 'feishu' ? this.sendFeishu(account, target, text, signal)
          : account.provider === 'wecom' ? this.sendWecom(account, target, text, signal)
            : account.provider === 'dingtalk' ? this.sendDingtalk(account, target, text, signal)
              : Promise.resolve({ outcome: 'failed', message: '当前渠道尚未实现发送', retryable: false });
      return await this.requestStorage.run(signal, () => Promise.race([delivery(), new Promise((resolve) => signal.addEventListener('abort', () => resolve({ outcome: 'unknown', message: '平台未确认发送结果' }), { once: true }))]));
    } catch (error) {
      if ([401, 403].includes(error?.status)) {
        account.status = 'auth_required';
        this.event('account', this.publicAccount(account));
        return { outcome: 'failed', message: '账号需要重新授权', retryable: false };
      }
      return { outcome: 'unknown', message: '平台未确认发送结果' };
    }
  }

  async sendWecom(account, target, text, signal) {
    if (signal.aborted) return { outcome: 'unknown', message: '平台未确认发送结果' };
    try {
      const response = await account.client.sendMessage(target.id, { msgtype: 'markdown', markdown: { content: text } });
      if (response?.errcode !== undefined && Number(response.errcode) !== 0) return { outcome: 'failed', message: `企业微信拒绝发送${numericCode(response.errcode) ? `（${numericCode(response.errcode)}）` : ''}`, retryable: false };
    } catch (error) {
      if (Number.isFinite(Number(error?.errcode)) && Number(error.errcode) !== 0) return { outcome: 'failed', message: `企业微信拒绝发送${numericCode(error.errcode) ? `（${numericCode(error.errcode)}）` : ''}`, retryable: false };
      throw error;
    }
    return { outcome: 'accepted', message: '平台已接受' };
  }

  async dingtalkAccessToken(account) {
    if (account.accessToken?.expiresAt > Date.now()) return account.accessToken.value;
    if (!account.accessTokenRequest) account.accessTokenRequest = (async () => {
      const requestedAt = Date.now();
      const response = await this.requestStorage.exit(() => this.fetch('https://api.dingtalk.com/v1.0/oauth2/accessToken', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ appKey: account.credentials.appId, appSecret: account.credentials.appSecret }),
        signal: AbortSignal.any([AbortSignal.timeout(20_000), ...(account.abort ? [account.abort.signal] : [])]),
      }));
      const auth = await response.json();
      if (!response.ok || !auth.accessToken) return null;
      account.accessToken = { value: auth.accessToken, expiresAt: requestedAt + Number(auth.expireIn ?? 0) * 1000 };
      return auth.accessToken;
    })();
    const request = account.accessTokenRequest;
    try { return await request; }
    finally { if (account.accessTokenRequest === request) account.accessTokenRequest = undefined; }
  }

  async sendDingtalk(account, target, text, signal) {
    const accessToken = await this.dingtalkAccessToken(account);
    if (signal.aborted) return { outcome: 'unknown', message: '平台未确认发送结果' };
    if (!accessToken) return { outcome: 'failed', message: '钉钉授权失败', retryable: true };
    const group = target.kind === 'chat';
    const response = await this.fetch(`https://api.dingtalk.com/v1.0/robot/${group ? 'groupMessages/send' : 'oToMessages/batchSend'}`, {
      method: 'POST', headers: { 'x-acs-dingtalk-access-token': accessToken, 'content-type': 'application/json' },
      body: JSON.stringify({ robotCode: account.credentials.appId, msgKey: 'sampleText', msgParam: JSON.stringify({ content: text }), ...(group ? { openConversationId: target.id } : { userIds: [target.id] }) }),
      signal,
    });
    const result = await response.json();
    if (result.code === 'staffId.notExisted') return { outcome: 'failed', message: '钉钉接收对象已失效。请打开编辑，向机器人发送一条私信，再选择新识别的个人对象。', retryable: false };
    return response.ok && [undefined, 0, '0'].includes(result.errcode) && [undefined, 0, '0'].includes(result.code)
      ? { outcome: 'accepted', message: '平台已接受' }
      : { outcome: 'failed', message: `钉钉拒绝发送${numericCode(result.errcode ?? result.code) ? `（${numericCode(result.errcode ?? result.code)}）` : ''}`, retryable: false };
  }

  async sendFeishu(account, target, text, signal) {
    if (signal.aborted) return { outcome: 'unknown', message: '平台未确认发送结果' };
    const response = await account.client.im.v1.message.create({
      params: { receive_id_type: target.kind === 'chat' ? 'chat_id' : 'open_id' },
      data: { receive_id: target.id, msg_type: 'text', content: JSON.stringify({ text }) },
    });
    return response?.code === 0 && response?.data?.message_id
      ? { outcome: 'accepted', message: '平台已接受' }
      : { outcome: 'failed', message: `平台拒绝发送${numericCode(response?.code) ? `（${numericCode(response.code)}）` : ''}`, retryable: false };
  }

  async sendWeixin(account, target, text, signal) {
    const response = await this.weixinApi.sendText({
      baseUrl: account.credentials.baseUrl, token: account.credentials.token,
      targetId: target.id, text,
      contextToken: account.credentials.contextTokens?.[target.id] ?? account.credentials.contextToken,
      signal,
    });
    return [undefined, 0, '0'].includes(response.ret) && [undefined, 0, '0'].includes(response.errcode)
      ? { outcome: 'accepted', message: '平台已接受' }
      : { outcome: 'failed', message: `微信 iLink 拒绝发送${numericCode(response.ret ?? response.errcode) ? `（${numericCode(response.ret ?? response.errcode)}）` : ''}`, retryable: false };
  }

  async close() {
    if (this.closing) return;
    this.closing = true;
    for (const id of this.bindings.keys()) this.cancelBinding(id);
    for (const account of this.accounts.values()) {
      this.stopAccount(account);
    }
    this.accounts.clear();
    let drainTimer;
    await Promise.race([Promise.allSettled([...this.inFlight]), new Promise((resolve) => { drainTimer = setTimeout(resolve, 2_000); })]);
    clearTimeout(drainTimer);
    axios.interceptors.request.eject(this.axiosInterceptorId);
    if (globalThis.fetch === this.fetch) globalThis.fetch = this.originalGlobalFetch;
    axios.defaults.proxy = this.originalAxiosDefaults.proxy;
    axios.defaults.httpAgent = this.originalAxiosDefaults.httpAgent;
    axios.defaults.httpsAgent = this.originalAxiosDefaults.httpsAgent;
  }
}

export async function run(input = process.stdin, runtime = new Runtime(), respond = write) {
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  const pending = new Set();
  for await (const line of lines) {
    if (!line.trim()) continue;
    const task = (async () => {
      let request;
      try {
        request = JSON.parse(line);
        const result = await runtime.call(request.method, request.params ?? {});
        respond({ id: request.id, result });
      } catch (error) {
        respond({ id: request?.id, error: { code: error?.code ?? -32603, message: error?.code === -32601 ? '未知方法' : error instanceof TypeError ? error.message : safeError(error) } });
      }
    })();
    pending.add(task);
    task.finally(() => pending.delete(task));
    let method;
    try { method = JSON.parse(line).method; } catch {}
    if (method === 'shutdown') break;
  }
  await runtime.close();
  let drainTimer;
  await Promise.race([Promise.allSettled([...pending]), new Promise((resolve) => { drainTimer = setTimeout(resolve, 2_000); })]);
  clearTimeout(drainTimer);
  // Bun 的 readline 结束迭代后仍保留 stdin 句柄；run 拥有输入的生命周期。
  lines.close();
  input.destroy();
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await run();
