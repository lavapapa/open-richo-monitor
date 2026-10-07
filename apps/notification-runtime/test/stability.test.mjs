import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter, getEventListeners } from 'node:events';
import { Runtime } from '../src/main.mjs';
import { createWeixinTextApi } from '../src/weixin-text-api.mjs';

test('微信长轮询正常超时保留游标，外部取消仍结束请求', async () => {
  const api = createWeixinTextApi(async () => { throw new DOMException('fixture', 'TimeoutError'); });
  assert.deepEqual(await api.getUpdates({ token: 'fixture', cursor: 'cursor' }), { ret: 0, msgs: [], get_updates_buf: 'cursor' });
  const controller = new AbortController();
  controller.abort(new DOMException('caller cancelled', 'TimeoutError'));
  await assert.rejects(api.getUpdates({ token: 'fixture', signal: controller.signal }), { name: 'TimeoutError' });
});

test('通知重试等待完成后释放监听器，取消后不留下等待任务', async (t) => {
  const runtime = new Runtime({ emit() {} });
  t.after(() => runtime.close());
  const controller = new AbortController();
  for (let i = 0; i < 100; i++) await runtime.sleep(0, controller.signal);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  const waiting = runtime.sleep(60_000, controller.signal);
  controller.abort();
  await assert.rejects(waiting, { name: 'AbortError' });
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  await assert.rejects(runtime.sleep(60_000, controller.signal), { name: 'AbortError' });
});

test('领取后掉线且未提交平台的消息返回等待语义，避免有限重试耗尽后丢失', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout', 'Date'] });
  let calls = 0;
  const runtime = new Runtime({ emit() {}, fetchImpl: async () => { calls++; return Response.json({ ret: 0 }); } });
  t.after(() => runtime.close());
  runtime.accounts.set('offline', { id: 'offline', provider: 'weixin', enabled: true, status: 'reconnecting', targets: [], credentials: {} });
  const sending = runtime.send({ accountId: 'offline', target: { id: 'owner', kind: 'user' }, text: 'fixture' });
  t.mock.timers.tick(15_001);
  runtime.accountChanges.emit('account:offline');
  const result = await sending;
  assert.equal(result.outcome, 'deferred');
  assert.equal(calls, 0);
});

async function pollBatches(t, batches, credentials = {}) {
  const runtime = new Runtime({ emit() {} });
  t.after(() => runtime.close());
  const account = { id: 'wx-fixture', provider: 'weixin', enabled: true, targets: [], credentials: { contextTokens: {}, ...credentials }, abort: new AbortController() };
  runtime.accounts.set(account.id, account);
  runtime.weixinApi.getUpdates = async () => {
    if (batches.length) return { ret: 0, msgs: batches.shift() };
    account.abort.abort();
    throw account.abort.signal.reason;
  };
  await runtime.pollWeixin(account);
  return account.credentials;
}

test('微信乱序及重复批次保留较新序号的会话令牌', async (t) => {
  const credentials = await pollBatches(t, [
    [{ from_user_id: 'owner', seq: '9007199254740993', context_token: 'new' }, { from_user_id: 'owner', seq: '9007199254740992', context_token: 'old' }],
    [{ from_user_id: 'owner', seq: '9007199254740993', context_token: 'duplicate' }],
  ]);
  assert.equal(credentials.contextTokens.owner, 'new');
  assert.equal(credentials.contextMetadata.owner.seq, '9007199254740993');
});

test('微信缺序号时以消息时间防止旧批次覆盖，恢复后沿用顺序信息', async (t) => {
  const credentials = await pollBatches(t, [[{ from_user_id: 'owner', create_time_ms: 1000, context_token: 'old' }]], {
    contextTokens: { owner: 'persisted' }, contextMetadata: { owner: { messageTimeMs: 2000 } },
  });
  assert.equal(credentials.contextTokens.owner, 'persisted');
});

test('微信上下文与顺序元数据共用一千项窗口，重开时截断过量存量', async (t) => {
  const messages = Array.from({ length: 1100 }, (_, index) => ({ from_user_id: `owner-${index}`, seq: String(index + 1), context_token: `context-${index}` }));
  const credentials = await pollBatches(t, [messages]);
  assert.equal(Object.keys(credentials.contextTokens).length, 1000);
  assert.equal(Object.keys(credentials.contextMetadata).length, 1000);
  assert.equal(credentials.contextTokens['owner-0'], undefined);
  assert.equal(credentials.contextMetadata['owner-0'], undefined);
  assert.equal(credentials.contextTokens['owner-1099'], 'context-1099');
  const restored = await pollBatches(t, [], { contextTokens: Object.fromEntries(messages.map(m => [m.from_user_id, m.context_token])), contextMetadata: Object.fromEntries(messages.map(m => [m.from_user_id, { seq: m.seq }])) });
  assert.equal(Object.keys(restored.contextTokens).length, 1000);
  assert.equal(Object.keys(restored.contextMetadata).length, 1000);
  const updated = await pollBatches(t, [[
    { from_user_id: 'new-owner', seq: '1', context_token: 'new-context' },
    { from_user_id: 'owner-100', seq: '2000', context_token: 'refreshed-context' },
  ]], credentials);
  assert.equal(updated.contextTokens['owner-100'], 'refreshed-context');
  assert.equal(updated.contextTokens['owner-101'], undefined);
});

test('飞书重复单聊映射不重复发布凭据，真实变化仍更新', async (t) => {
  const events = [];
  class WS { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} }
  class Dispatcher { register(handlers) { this.handlers = handlers; return this; } }
  const runtime = new Runtime({ emit: event => events.push(event), sdk: { FeishuClient: class {}, FeishuWSClient: WS, FeishuEventDispatcher: Dispatcher } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [{ id: 'feishu', provider: 'feishu', enabled: true, credentials: { appId: 'fixture', appSecret: 'fixture' }, targets: [] }] });
  const handler = runtime.accounts.get('feishu').dispatcher.handlers['im.chat.access_event.bot_p2p_chat_entered_v1'];
  for (let i = 0; i < 100; i++) handler({ operator_id: { open_id: 'owner' }, chat_id: 'chat' });
  assert.equal(events.filter(event => event.event === 'credentials' && event.data.credentials.p2pChatIds).length, 1);
  handler({ operator_id: { open_id: 'owner' }, chat_id: 'new-chat' });
  assert.equal(events.filter(event => event.event === 'credentials' && event.data.credentials.p2pChatIds).length, 2);
});

class WecomFake extends EventEmitter {
  connect() { this.isConnected = true; this.emit('connected'); }
  disconnect() { this.isConnected = false; }
}
async function wecomFixture(t) {
  const runtime = new Runtime({ emit() {}, sdk: { WecomWSClient: WecomFake } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [{ id: 'wecom', provider: 'wecom', enabled: true, credentials: { botId: 'fixture', secret: 'fixture' }, targets: [] }] });
  await new Promise(resolve => setImmediate(resolve));
  return { runtime, account: runtime.accounts.get('wecom') };
}

test('企微 socket 打开后等待认证成功，重连同样重新核验认证', async (t) => {
  const { runtime, account } = await wecomFixture(t);
  runtime.syncAccountStatus(account);
  assert.equal(account.status, 'connecting');
  account.client.emit('authenticated');
  assert.equal(account.status, 'ready');
  account.client.emit('disconnected', 'network');
  account.client.emit('connected');
  runtime.syncAccountStatus(account);
  assert.notEqual(account.status, 'ready');
  account.client.emit('authenticated');
  assert.equal(account.status, 'ready');
});

test('企微被其他实例接管后停止争抢，明确状态且可人工重连', async (t) => {
  const { runtime, account } = await wecomFixture(t);
  const old = account.client;
  old.emit('authenticated');
  old.emit('event.disconnected_event', { body: { event: { eventtype: 'disconnected_event' } } });
  old.emit('disconnected', 'New connection established');
  runtime.syncAccountStatus(account);
  assert.equal(account.status, 'connection_conflict');
  assert.equal(account.supervisor, undefined);
  await runtime.restartAccount(account);
  assert.notEqual(account.client, old);
  account.client.emit('authenticated');
  assert.equal(account.status, 'ready');
});

test('企微认证等待超时后关闭旧连接并进入有间隔恢复', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const { account } = await wecomFixture(t);
  const old = account.client;
  t.mock.timers.tick(20_001);
  assert.equal(account.status, 'failed');
  assert.equal(old.isConnected, false);
  assert.ok(account.retryAt);
});

test('官方飞书长连接端点认证拒绝停止重连，保留认证状态', async (t) => {
  for (const http of [false, true]) {
    const runtime = new Runtime({ emit() {}, sdk: { FeishuClient: class {} } });
    t.after(() => runtime.close());
    const OfficialWS = runtime.sdk.FeishuWSClient;
    runtime.sdk.FeishuWSClient = class extends OfficialWS {
      constructor(options) {
        options.httpInstance.defaults.adapter = async config => {
          assert.ok(config.url.endsWith('/callback/ws/endpoint'));
          if (http) throw Object.assign(new Error('fixture'), { response: { status: 401, config }, config });
          return { data: { code: 514, data: {}, msg: 'fixture' }, status: 200, headers: {}, config };
        };
        super(options);
      }
    };
    runtime.configure({ accounts: [{ id: 'feishu', provider: 'feishu', enabled: true, credentials: { appId: 'cli_0123456789abcdef', appSecret: 'fixture' }, targets: [] }] });
    for (let i = 0; i < 100 && runtime.accounts.get('feishu').status !== 'auth_required'; i++) await new Promise(resolve => setImmediate(resolve));
    const account = runtime.accounts.get('feishu');
    assert.equal(account.status, 'auth_required');
    assert.equal(account.supervisor, undefined);
    await runtime.close();
  }
});

test('飞书启动启用 SDK 的失去心跳响应检测', async (t) => {
  let options;
  const runtime = new Runtime({ emit() {}, sdk: { FeishuClient: class {}, FeishuWSClient: class { constructor(o) { options = o; } start() { options.onReady(); } close() {} } } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [{ id: 'feishu', provider: 'feishu', enabled: true, credentials: { appId: 'fixture', appSecret: 'fixture' }, targets: [] }] });
  assert.equal(options.wsConfig.pingTimeout, 15);
});

test('官方飞书心跳超时终止半开连接，有入站响应与关闭时取消检测', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  const runtime = new Runtime({ emit() {} });
  t.after(() => runtime.close());
  const ws = new runtime.sdk.FeishuWSClient({ appId: 'cli_0123456789abcdef', appSecret: 'fixture', wsConfig: { pingTimeout: 15 } });
  let terminated = 0;
  ws.wsConfig.setWSInstance({ terminate() { terminated++; }, close() {}, removeAllListeners() {} });
  ws.armLiveness();
  t.mock.timers.tick(14_999);
  assert.equal(terminated, 0);
  ws.clearLiveness();
  t.mock.timers.tick(15_001);
  assert.equal(terminated, 0);
  ws.armLiveness();
  t.mock.timers.tick(15_001);
  assert.equal(terminated, 1);
  ws.armLiveness();
  ws.close();
  t.mock.timers.tick(15_001);
  assert.equal(terminated, 1);
});

function dingtalkFixture(t, fetchImpl) {
  const runtime = new Runtime({ emit() {}, fetchImpl });
  t.after(() => runtime.close());
  const account = { id: 'ding', provider: 'dingtalk', enabled: true, status: 'ready', credentials: { appId: 'fixture', appSecret: 'fixture' }, targets: [], accessToken: { value: 'old', expiresAt: Date.now() + 30_000 } };
  runtime.accounts.set(account.id, account);
  const send = () => runtime.send({ accountId: 'ding', target: { id: 'group', kind: 'chat' }, text: '测试' });
  return { runtime, account, send };
}

test('钉钉一百个并发令牌请求共用一次刷新，结束后释放请求', async (t) => {
  let requests = 0;
  const { runtime, account } = dingtalkFixture(t, async () => {
    requests++;
    await new Promise(resolve => setImmediate(resolve));
    return Response.json({ accessToken: 'new', expireIn: 7200 });
  });
  const tokens = await Promise.all(Array.from({ length: 100 }, () => runtime.dingtalkAccessToken(account)));
  assert.equal(requests, 1);
  assert.ok(tokens.every(token => token === 'new'));
  assert.equal(account.accessTokenRequest, undefined);
});

test('钉钉临近到期主动刷新，服务端明确拒绝旧令牌后仅刷新重发一次', async (t) => {
  let auth = 0, deliveries = 0;
  const { account, send } = dingtalkFixture(t, async url => {
    if (String(url).endsWith('/accessToken')) { auth++; return Response.json({ accessToken: 'new', expireIn: 7200 }); }
    deliveries++;
    return deliveries === 2 ? Response.json({ code: 'InvalidAuthentication' }, { status: 401 }) : Response.json({ processQueryKey: 'accepted' });
  });
  assert.equal((await send()).outcome, 'accepted');
  assert.equal(auth, 1);
  assert.equal((await send()).outcome, 'accepted');
  assert.equal(auth, 2);
  assert.equal(deliveries, 3);
  assert.equal(account.status, 'ready');
});

test('钉钉鉴权拒绝转重新授权，明确限流保留可重试语义', async (t) => {
  const denied = dingtalkFixture(t, async () => Response.json({ code: 'InvalidAuthentication' }, { status: 401 }));
  assert.equal((await denied.send()).retryable, false);
  assert.equal(denied.account.status, 'auth_required');
  const limited = dingtalkFixture(t, async url => String(url).endsWith('/accessToken') ? Response.json({ accessToken: 'new', expireIn: 7200 }) : Response.json({ code: 'Throttling' }, { status: 429 }));
  assert.equal((await limited.send()).retryable, true);
});

test('钉钉断线后及时重建连接，失败仍遵从恢复间隔', async (t) => {
  const originalNow = Date.now;
  let clock = originalNow(), connects = 0;
  Date.now = () => clock;
  t.after(() => { Date.now = originalNow; });
  class DingFake extends EventEmitter {
    async connect() { connects++; this.connected = true; this.emit('connected'); }
    disconnect() { this.connected = false; }
    registerCallbackListener() {}
  }
  const runtime = new Runtime({ emit() {}, sdk: { DWClient: DingFake } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [{ id: 'ding', provider: 'dingtalk', enabled: true, credentials: { appId: 'fixture', appSecret: 'fixture' }, targets: [] }] });
  await new Promise(resolve => setImmediate(resolve));
  const account = runtime.accounts.get('ding');
  account.client.connected = false;
  runtime.syncAccountStatus(account);
  clock += 5001;
  runtime.syncAccountStatus(account);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(connects, 2);
  assert.equal(account.status, 'ready');
});
