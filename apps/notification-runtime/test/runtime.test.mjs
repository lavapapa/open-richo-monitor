import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter, once } from 'node:events';
import { createServer } from 'node:http';
import { PassThrough } from 'node:stream';
import https from 'node:https';
import axios from 'axios';
import { WebSocketServer } from 'ws';
import { WSClient as OfficialWecomWSClient } from '@wecom/aibot-node-sdk';
import { DWClient as OfficialDingTalkClient, TOPIC_ROBOT } from 'dingtalk-stream';
import { Runtime, run } from '../src/main.mjs';

test('shutdown RPC 返回响应后释放输入流，父进程无需再发送 EOF', async () => {
  const input = new PassThrough();
  const responses = [];
  const runtime = new Runtime({ emit: () => {} });
  const running = run(input, runtime, (value) => responses.push(value));
  input.write(`${JSON.stringify({ id: 1, method: 'shutdown' })}\n`);
  await running;
  assert.deepEqual(responses, [{ id: 1, result: { stopped: true } }]);
  assert.equal(input.destroyed, true, 'run 应释放其拥有的输入句柄');
});

test('企微扫码事件公开二维码而凭据仅从 binding_status 返回', async () => {
  const events = [];
  let polls = 0;
  const runtime = new Runtime({
    emit: (event) => events.push(event),
    sleepImpl: async () => {},
    fetchImpl: async (input) => {
      const url = new URL(input);
      if (url.pathname.endsWith('/generate')) return Response.json({ data: { scode: 'private-code', auth_url: 'https://work.weixin.qq.com/ai/qr' } });
      polls += 1;
      return Response.json({ data: { status: 'success', bot_info: { botid: 'bot-1', secret: 'secret-1' } } });
    },
  });
  const started = await runtime.call('begin_binding', { provider: 'wecom', bindingId: 'bind-1' });
  assert.equal(started.status, 'waiting');
  for (let i = 0; i < 100 && !runtime.bindings.get('bind-1').qrUrl; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(runtime.bindings.get('bind-1').qrUrl, 'https://work.weixin.qq.com/ai/qr');
  for (let i = 0; i < 100 && runtime.bindings.get('bind-1').status !== 'complete'; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
  const completed = await runtime.call('binding_status', { bindingId: 'bind-1' });
  assert.deepEqual(completed.credentials, { botId: 'bot-1', secret: 'secret-1' });
  assert.equal(polls, 1);
  assert.equal(JSON.stringify(events).includes('secret-1'), false);
  await runtime.close();
});

test('configure 拒绝已移除的 QQ 渠道，且不会启动账号', async () => {
  let started = false;
  const runtime = new Runtime({ emit: () => {}, sdk: { QQBot: class { constructor() { started = true; } } } });
  await assert.rejects(runtime.call('configure', { accounts: [{ id: 'qq', provider: 'qq', credentials: { appId: 'id', appSecret: 'secret' }, targets: [], enabled: true }] }), /渠道不受支持/);
  assert.equal(started, false);
  assert.equal(runtime.accounts.size, 0);
  await runtime.close();
});

test('企微和钉钉绑定群组检测使用已发现群，保留个人目标；微信不支持群组检测', async () => {
  const runtime = new Runtime({ emit: () => {} });
  for (const provider of ['wecom', 'dingtalk']) {
    const id = `${provider}-binding`;
    const targets = [{ id: 'person-1', kind: 'user' }, { id: 'group-1', kind: 'chat' }];
    runtime.bindings.set(id, { id, provider, status: 'complete', targets: [...targets] });
    runtime.accounts.set(id, { id, provider, status: 'ready', enabled: true, targets: [...targets] });
    assert.deepEqual(await runtime.call('detect_binding_groups', { bindingId: id }), { targets: [targets[1]] });
    assert.deepEqual(runtime.bindings.get(id).targets, targets);
  }
  const wx = { id: 'wx-binding', provider: 'weixin', status: 'complete', targets: [] };
  runtime.bindings.set(wx.id, wx);
  runtime.accounts.set(wx.id, { id: wx.id, provider: 'weixin', status: 'ready', enabled: true, targets: [] });
  await assert.rejects(runtime.call('detect_binding_groups', { bindingId: wx.id }), /绑定尚未完成或不支持群组检测/);
  await runtime.close();
});

test('configure 与 shutdown 保持逐行契约，状态不包含凭据', async () => {
  const output = [];
  const runtime = new Runtime({ emit: (value) => output.push(value) });
  await runtime.call('configure', { accounts: [{ id: 'a', provider: 'feishu', credentials: { appId: 'id', appSecret: 'secret' }, targets: [], enabled: false }] });
  const status = await runtime.call('status');
  assert.equal(status.accounts[0].id, 'a');
  assert.equal(JSON.stringify(status).includes('secret'), false);
  await runtime.call('shutdown');
  assert.deepEqual(runtime.accounts, new Map());
});

test('飞书绑定向 SDK 传出二维码后返回 Core 可保存的凭据', async () => {
  const events = [];
  let options;
  const runtime = new Runtime({
    emit: (event) => events.push(event),
    register: async (value) => {
      options = value;
      value.onQRCodeReady({ url: 'https://accounts.feishu.cn/qr', expireIn: 600 });
      return { client_id: 'cli-id', client_secret: 'app-secret', user_info: { open_id: 'ou_owner', tenant_brand: 'feishu' } };
    },
  });
  await runtime.call('begin_binding', { provider: 'feishu', bindingId: 'feishu-1' });
  for (let i = 0; i < 100 && runtime.bindings.get('feishu-1').status !== 'complete'; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(options.createOnly, true);
  assert.ok(options.addons.scopes.tenant.includes('im:chat.access_event.bot_p2p_chat:read'));
  assert.ok(options.addons.events.items.tenant.includes('im.chat.access_event.bot_p2p_chat_entered_v1'));
  assert.deepEqual((await runtime.call('binding_status', { bindingId: 'feishu-1' })).credentials, { appId: 'cli-id', appSecret: 'app-secret', domain: 'feishu', userOpenId: 'ou_owner', botUrl: 'https://applink.feishu.cn/client/bot/open?appId=cli-id' });
  assert.equal(JSON.stringify(events).includes('app-secret'), false);
  await runtime.close();
});

test('configure 复用飞书连接并将 chat 目标映射到 chat_id 发送', async () => {
  const events = [];
  const sent = [];
  let clientCount = 0;
  let startCount = 0;
  let closed = false;
  class FakeClient {
    constructor() { clientCount += 1; this.im = { v1: { message: { create: async (request) => { sent.push(request); return { code: 0, data: { message_id: 'm1' } }; } } } }; }
  }
  class FakeWs {
    constructor(options) { this.options = options; }
    start() { startCount += 1; this.options.onReady(); }
    getConnectionStatus() { return { state: 'connected' }; }
    close() { closed = true; }
  }
  class FakeDispatcher { register(handlers) { this.handlers = handlers; return this; } }
  const runtime = new Runtime({ emit: (event) => events.push(event), sdk: { FeishuClient: FakeClient, FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  const params = { accounts: [{ id: 'f1', provider: 'feishu', credentials: { appId: 'app', appSecret: 'secret' }, targets: [], enabled: true }] };
  await runtime.call('configure', params);
  await new Promise((resolve) => setTimeout(resolve, 0));
  await runtime.call('configure', params);
  const result = await runtime.call('send', { accountId: 'f1', target: { id: 'chat-id', kind: 'chat' }, text: '库存更新' });
  assert.deepEqual(result, { outcome: 'accepted', message: '平台已接受' });
  assert.equal(clientCount, 1);
  assert.equal(startCount, 1);
  assert.equal(sent[0].params.receive_id_type, 'chat_id');
  await runtime.close();
  assert.equal(closed, true);
  assert.equal(events.some((event) => event.event === 'account' && event.data.status === 'ready'), true);
});

test('飞书官方机器人名称在扫码完成状态公开，凭据保持私有', async () => {
  const events = [];
  class FakeClient { async request(request) { assert.equal(request.url, 'https://open.feishu.cn/open-apis/bot/v3/info/'); return { code: 0, bot: { app_name: '理光库存助手' } }; } }
  class FakeWs { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
  class FakeDispatcher { register() { return this; } }
  const runtime = new Runtime({ emit: (event) => events.push(event), sdk: { FeishuClient: FakeClient, FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  const binding = { id: 'named-bot', provider: 'feishu', status: 'complete', credentials: { appId: 'fixture', appSecret: 'private-secret' }, targets: [], controller: new AbortController() };
  runtime.bindings.set(binding.id, binding);
  await runtime.startProvisionalBinding(binding);
  await new Promise((resolve) => setTimeout(resolve, 0));
  const publicBinding = runtime.publicBinding(binding);
  assert.equal(publicBinding.botName, '理光库存助手');
  assert.equal(JSON.stringify(publicBinding).includes('private-secret'), false);
  assert.equal(events.some((event) => event.event === 'credentials' && event.data.credentials.botName === '理光库存助手'), true);
  await runtime.close();
});

test('微信 iLink 轮询上报游标与新的上下文凭据，不输出令牌', async () => {
  const events = [];
  let calls = 0;
  const runtime = new Runtime({
    emit: (event) => events.push(event),
    fetchImpl: async (_input, options) => {
      if (String(_input).includes('notifystart') || String(_input).includes('notifystop')) return Response.json({ ret: 0 });
      calls += 1;
      if (calls === 1) return Response.json({ get_updates_buf: 'cursor-2', msgs: [{ from_user_id: 'user-1', context_token: 'context-secret' }] });
      await new Promise((resolve) => options.signal.addEventListener('abort', resolve, { once: true }));
      throw new DOMException('aborted', 'AbortError');
    },
  });
  await runtime.call('configure', { accounts: [{ id: 'wx1', provider: 'weixin', credentials: { botToken: 'bot-secret', accountId: 'bot-id', userId: 'owner' }, targets: [], enabled: true }] });
  for (let i = 0; i < 100 && !events.some((event) => event.event === 'credentials'); i += 1) await new Promise((resolve) => setTimeout(resolve, 1));
  const update = events.find((event) => event.event === 'credentials');
  assert.equal(update.data.accountId, 'wx1');
  assert.deepEqual(update.data.credentials, { getUpdatesBuf: 'cursor-2', contextTokens: { 'user-1': 'context-secret' } });
  assert.equal(JSON.stringify(events.filter((event) => event.event !== 'credentials')).includes('bot-secret'), false);
  await runtime.close();
});

test('飞书群组发现使用分页结果并发布 chat targets', async () => {
  const events = [];
  const requests = [];
  class FakeClient {
    constructor() {
      this.im = { v1: { chat: { list: async ({ params }) => {
        requests.push(params);
        return params.page_token
          ? { code: 0, data: { has_more: false, items: [{ chat_id: 'group-2', name: '库存群', chat_mode: 'group' }] } }
          : { code: 0, data: { has_more: true, page_token: 'next', items: [{ chat_id: 'group-1', name: '值班群', chat_mode: 'group' }, { chat_id: 'p2p', chat_mode: 'p2p' }] } };
      } } } };
    }
  }
  class FakeWs { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
  class FakeDispatcher { register() { return this; } }
  const runtime = new Runtime({ emit: (event) => events.push(event), sdk: { FeishuClient: FakeClient, FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  await runtime.call('configure', { accounts: [{ id: 'f-groups', provider: 'feishu', credentials: { appId: 'app', appSecret: 'secret' }, targets: [], enabled: true }] });
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(await runtime.call('detect_groups', { accountId: 'f-groups' }), { targets: [
    { id: 'group-1', kind: 'chat', label: '值班群' }, { id: 'group-2', kind: 'chat', label: '库存群' },
  ] });
  assert.equal(requests[0].page_size, 100);
  assert.equal(requests[1].page_token, 'next');
  assert.equal(events.filter((event) => event.event === 'target').length, 2);
  await runtime.close();
});

test('会话更新保留已知群名，并同步真实名称变化', async () => {
  const events = [];
  const runtime = new Runtime({ emit: (event) => events.push(event) });
  const account = { id: 'names', provider: 'dingtalk', enabled: true, status: 'ready', targets: [] };
  runtime.accounts.set(account.id, account);
  runtime.discoverTarget(account, { id: 'cid-1', kind: 'chat', label: '测试群' });
  runtime.discoverTarget(account, { id: 'cid-1', kind: 'chat', label: 'cid-1' });
  assert.deepEqual(account.targets, [{ id: 'cid-1', kind: 'chat', label: '测试群' }]);
  assert.equal(events.filter((event) => event.event === 'target').at(-1).data.target.label, '测试群');
  runtime.discoverTarget(account, { id: 'cid-1', kind: 'chat', label: '新群名' });
  assert.equal(account.targets[0].label, '新群名');
  runtime.discoverTarget(account, { id: 'cid-1', kind: 'user', label: '成员' });
  assert.equal(account.targets.length, 2);
  await runtime.close();
});

test('复用连接重新配置时保留已发现但未勾选的群聊', async () => {
  const runtime = new Runtime({ emit: () => {} });
  const credentials = { appId: 'app', appSecret: 'fixture-secret' };
  const selected = { id: 'group-a', kind: 'chat', label: '群 A' };
  const discovered = { id: 'group-b', kind: 'chat', label: '群 B' };
  const client = { disconnect() {} };
  runtime.accounts.set('known', { id: 'known', provider: 'dingtalk', enabled: true,
    credentials, transportKey: 'direct', status: 'ready', client, targets: [selected, discovered] });
  await runtime.call('configure', { accounts: [{ id: 'known', provider: 'dingtalk', enabled: true,
    credentials, targets: [selected] }] });
  assert.equal(runtime.accounts.get('known').client, client);
  assert.deepEqual((await runtime.call('detect_groups', { accountId: 'known' })).targets, [selected, discovered]);
  await runtime.close();
});

test('凭据字段顺序变化及反复配置不会重建连接', async () => {
  const runtime = new Runtime({ emit: () => {} });
  let disconnected = 0;
  const client = { disconnect() { disconnected += 1; } };
  runtime.accounts.set('order', { id: 'order', provider: 'dingtalk', enabled: true,
    credentials: { appSecret: 's', appId: 'app' }, transportKey: 'direct', status: 'ready', client, targets: [] });
  for (let i = 0; i < 100; i += 1) {
    await runtime.configure({ accounts: [{ id: 'order', provider: 'dingtalk', enabled: true,
      credentials: { appId: 'app', appSecret: 's' }, targets: [] }] });
    assert.equal(runtime.accounts.get('order').client, client);
    assert.equal(runtime.accounts.get('order').status, 'ready');
  }
  assert.equal(disconnected, 0);
  await runtime.close();
});

test('微信默认值和运行中游标更新不触发重连，旧配置保留最新上下文', async () => {
  let calls = 0;
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async (_input, options) => {
    if (String(_input).includes('notifystart') || String(_input).includes('notifystop')) return Response.json({ ret: 0 });
    calls += 1;
    if (calls === 1) return Response.json({ get_updates_buf: 'fresh-cursor', msgs: [{ from_user_id: 'user', context_token: 'fresh-context' }] });
    await new Promise((resolve) => options.signal.addEventListener('abort', resolve, { once: true }));
    throw new DOMException('aborted', 'AbortError');
  } });
  const config = { accounts: [{ id: 'wx-stable', provider: 'weixin', credentials: { botToken: 'fixture-token', userId: 'user' }, targets: [], enabled: true }] };
  try {
    await runtime.configure(structuredClone(config));
    for (let i = 0; i < 20 && calls < 2; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
    const account = runtime.accounts.get('wx-stable');
    const transport = account.abort;
    for (let i = 0; i < 100; i += 1) await runtime.configure(structuredClone(config));
    assert.equal(runtime.accounts.get('wx-stable'), account);
    assert.equal(account.abort, transport);
    assert.equal(transport.signal.aborted, false);
    assert.equal(account.credentials.contextTokens.user, 'fresh-context');
    assert.equal(account.credentials.getUpdatesBuf, 'fresh-cursor');
    assert.equal(calls, 2);
  } finally { await runtime.close(); }
});

test('手动更换微信 botToken 覆盖运行时 token 别名并重建连接', async () => {
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async (_input, options) => {
    await new Promise((resolve) => options.signal.addEventListener('abort', resolve, { once: true }));
    throw new DOMException('aborted', 'AbortError');
  } });
  try {
    const controller = new AbortController();
    runtime.accounts.set('wx-change', { id: 'wx-change', provider: 'weixin', enabled: true, status: 'ready', transportKey: 'direct', abort: controller, targets: [], credentials: { botToken: 'old', token: 'old', baseUrl: 'https://ilinkai.weixin.qq.com/' } });
    await runtime.configure({ accounts: [{ id: 'wx-change', provider: 'weixin', enabled: true, targets: [], credentials: { botToken: 'new', token: 'old', baseUrl: 'https://ilinkai.weixin.qq.com/' } }] });
    assert.equal(controller.signal.aborted, true);
    assert.equal(runtime.accounts.get('wx-change').credentials.token, 'new');
  } finally { await runtime.close(); }
});

test('连接就绪事件立即释放等待中的发送，不等待 100 毫秒轮询', async () => {
  const runtime = new Runtime({emit:()=>{}});
  const account = {id:'error',provider:'feishu',status:'connecting',enabled:true,credentials:{},targets:[],client:{im:{v1:{message:{create:async()=>({code:0,data:{message_id:'accepted'}})}}}}};
  runtime.accounts.set(account.id,account);
  try {
    const sending = runtime.call('send',{accountId:account.id,target:{id:'chat',kind:'chat'},text:'库存'});
    account.status = 'ready';
    runtime.event('account',runtime.publicAccount(account));
    let timer;
    const result = await Promise.race([sending,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('仍在等待轮询')),70);})]).finally(()=>clearTimeout(timer));
    assert.equal(result.outcome,'accepted');
  } finally {await runtime.close();}
});

test('钉钉并行和连续发送复用未过期的 accessToken，过期后重新获取', async () => {
  let authCalls = 0;
  const tokens = [];
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async (url, options) => {
    if (String(url).endsWith('/oauth2/accessToken')) {
      authCalls += 1;
      await new Promise((resolve) => setTimeout(resolve, 10));
      return Response.json({ accessToken: `token-${authCalls}`, expireIn: 7200 });
    }
    tokens.push(options.headers['x-acs-dingtalk-access-token']);
    return Response.json({ processQueryKey: 'accepted' });
  }});
  const account = { id:'dt-token',provider:'dingtalk',credentials:{appId:'app',appSecret:'secret'},targets:[],enabled:true,status:'ready' };
  runtime.accounts.set(account.id, account);
  const send = (id) => runtime.call('send', {accountId:account.id,target:{id,kind:'chat'},text:'库存提醒'});
  assert.ok((await Promise.all([send('a'),send('b')])).every((result) => result.outcome === 'accepted'));
  await send('a');
  assert.equal(authCalls,1);
  assert.deepEqual(tokens,['token-1','token-1','token-1']);
  account.accessToken.expiresAt = Date.now() - 1;
  await send('b');
  assert.equal(authCalls,2);
  await runtime.close();
});

test('取消一条钉钉发送不取消其他发送共用的授权请求，账户停用仍会取消授权', async () => {
  let completeAuth;
  let authSignal;
  const runtime = new Runtime({emit:()=>{}, fetchImpl:async (url, options) => {
    if (!String(url).endsWith('/oauth2/accessToken')) return Response.json({processQueryKey:'ok'});
    authSignal = options.signal;
    return new Promise((resolve,reject) => {
      completeAuth = () => resolve(Response.json({accessToken:'shared',expireIn:7200}));
      options.signal.addEventListener('abort', () => reject(options.signal.reason),{once:true});
    });
  }});
  const account = {id:'dt-cancel',provider:'dingtalk',credentials:{appId:'app',appSecret:'secret'},targets:[],enabled:true,status:'ready',abort:new AbortController()};
  runtime.accounts.set(account.id, account);
  const send = (id) => runtime.call('send',{accountId:account.id,target:{id,kind:'chat'},text:'提醒'});
  const first = send('a');
  const firstController = [...account.sendControllers][0];
  const second = send('b');
  firstController.abort();
  assert.equal((await first).outcome,'unknown');
  assert.equal(authSignal.aborted,false);
  completeAuth();
  assert.equal((await second).outcome,'accepted');
  account.accessToken.expiresAt = 0;
  const pending = send('c');
  await runtime.close();
  assert.equal(authSignal.aborted,true);
  assert.equal((await pending).outcome,'unknown');
});

test('钉钉 staffId.notExisted 提示重新发现个人接收对象且不可重试', async () => {
  let calls = 0;
  const runtime = new Runtime({
    emit: () => {},
    fetchImpl: async (input) => {
      calls += 1;
      if (String(input).endsWith('/oauth2/accessToken')) return Response.json({ accessToken: 'access-token' });
      return Response.json({ code: 'staffId.notExisted' }, { status: 400 });
    },
  });
  runtime.accounts.set('dt-send', { id: 'dt-send', provider: 'dingtalk', credentials: { appId: 'app', appSecret: 'secret' }, targets: [], enabled: true, status: 'ready' });
  assert.deepEqual(await runtime.call('send', { accountId: 'dt-send', target: { id: '$old-target', kind: 'user' }, text: 'hello' }), {
    outcome: 'failed', message: '钉钉接收对象已失效。请打开编辑，向机器人发送一条私信，再选择新识别的个人对象。', retryable: false,
  });
  assert.equal(calls, 2);
  await runtime.close();
});

test('Feishu SDK 明确拒绝授权后进入 auth_required，不安排网络重试', async () => {
  class FakeClient { constructor() { this.im = { v1: { message: { create: async () => ({}) } } }; } }
  class FakeWs {
    constructor(options) { this.options = options; }
    start() { this.options.onError(Object.assign(new Error('unauthorized'), { statusCode: 401, name: 'AccessTokenError' })); }
    close() {}
  }
  class FakeDispatcher { register() { return this; } }
  const runtime = new Runtime({ emit: () => {}, sdk: { FeishuClient: FakeClient, FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  await runtime.call('configure', { accounts: [{ id: 'bad-auth', provider: 'feishu', credentials: { appId: 'id', appSecret: 'bad' }, targets: [], enabled: true }] });
  assert.equal(runtime.accounts.get('bad-auth').status, 'auth_required');
  assert.equal(runtime.accounts.get('bad-auth').retryAt, undefined);
  await runtime.close();
});

test('企微 SDK 的重连耗尽会被外层监督器重启，认证耗尽则停止重试', async () => {
  const clients = [];
  class FakeWecom extends EventEmitter {
    constructor() { super(); clients.push(this); }
    connect() {}
    disconnect() {}
    get isConnected() { return false; }
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { WecomWSClient: FakeWecom } });
  await runtime.call('configure', { accounts: [{ id: 'wxwork', provider: 'wecom', credentials: { botId: 'id', secret: 's' }, targets: [], enabled: true }] });
  clients[0].emit('error', Object.assign(new Error('network'), { name: 'WSReconnectExhaustedError' }));
  assert.equal(runtime.accounts.get('wxwork').status, 'failed');
  runtime.accounts.get('wxwork').retryAt = Date.now() - 1;
  await runtime.restartAccount(runtime.accounts.get('wxwork'));
  assert.equal(clients.length, 2);
  clients[1].emit('error', Object.assign(new Error('auth'), { name: 'WSAuthFailureError' }));
  assert.equal(runtime.accounts.get('wxwork').status, 'auth_required');
  clients[1].emit('error', Object.assign(new Error('network'), { name: 'WSReconnectExhaustedError' }));
  assert.equal(runtime.accounts.get('wxwork').status, 'auth_required');
  assert.equal(runtime.accounts.get('wxwork').supervisor, undefined);
  await runtime.close();
});

test('停用或替换后，旧企微、飞书和钉钉 SDK 回调不再更新账号状态或 targets', async () => {
  const runtimes = [];
  let oldWecom;
  class FakeWecom extends EventEmitter {
    constructor() { super(); oldWecom = this; }
    connect() {}
    disconnect() {}
    get isConnected() { return false; }
  }
  const wecom = new Runtime({ emit: (event) => runtimes.push(event), sdk: { WecomWSClient: FakeWecom } });
  await wecom.call('configure', { accounts: [{ id: 'same', provider: 'wecom', credentials: { botId: 'id', secret: 's' }, targets: [], enabled: true }] });
  await wecom.call('configure', { accounts: [{ id: 'same', provider: 'wecom', credentials: { botId: 'id', secret: 's' }, targets: [], enabled: false }] });
  runtimes.length = 0;
  oldWecom.emit('authenticated');
  oldWecom.emit('error', new Error('late error'));
  oldWecom.emit('reconnecting');
  oldWecom.emit('disconnected');
  oldWecom.emit('close');
  oldWecom.emit('message', { body: { chatid: 'stale-group' } });
  assert.equal(wecom.accounts.get('same').status, 'stopped');
  assert.deepEqual(wecom.accounts.get('same').targets, []);
  assert.deepEqual(runtimes, []);
  await wecom.close();

  let oldFeishu;
  class FakeClient { constructor() { this.im = {}; } }
  class FakeWs { constructor(options) { this.options = options; oldFeishu = this; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
  class FakeDispatcher { register(handlers) { this.handlers = handlers; return this; } }
  const feishu = new Runtime({ emit: (event) => runtimes.push(event), sdk: { FeishuClient: FakeClient, FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  await feishu.call('configure', { accounts: [{ id: 'same', provider: 'feishu', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  await new Promise((resolve) => setTimeout(resolve, 0));
  await feishu.call('configure', { accounts: [{ id: 'same', provider: 'feishu', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: false }] });
  runtimes.length = 0;
  oldFeishu.options.onReady();
  oldFeishu.options.onReconnecting();
  oldFeishu.options.onReconnected();
  oldFeishu.options.onError(new Error('late error'));
  oldFeishu.dispatcher?.handlers?.['im.message.receive_v1']?.({ message: { chat_type: 'group', chat_id: 'stale-group' } });
  assert.equal(feishu.accounts.get('same').status, 'stopped');
  assert.deepEqual(feishu.accounts.get('same').targets, []);
  assert.deepEqual(runtimes, []);
  await feishu.close();

  let oldDing;
  class FakeDing extends EventEmitter {
    constructor() { super(); oldDing = this; this.connected = true; }
    registerCallbackListener() {}
    async connect() {}
    disconnect() { this.connected = false; }
    socketCallBackResponse() {}
  }
  const ding = new Runtime({ emit: (event) => runtimes.push(event), sdk: { DWClient: FakeDing } });
  await ding.call('configure', { accounts: [{ id: 'same', provider: 'dingtalk', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  await new Promise((resolve) => setTimeout(resolve, 0));
  await ding.call('configure', { accounts: [{ id: 'same', provider: 'dingtalk', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: false }] });
  runtimes.length = 0;
  oldDing.emit('connected');
  oldDing.emit('disconnected');
  assert.equal(ding.accounts.get('same').status, 'stopped');
  assert.deepEqual(runtimes, []);
  await ding.close();

});

test('钉钉 readiness 由已完成鉴权的 connected 状态驱动，不等待 REGISTERED', async () => {
  let client;
  class FakeDingTalk extends EventEmitter {
    constructor(options) { super(); client = this; this.options = options; this.connected = true; this.registered = false; }
    registerCallbackListener() {}
    async connect() {}
    disconnect() { this.connected = false; }
    socketCallBackResponse() {}
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { DWClient: FakeDingTalk } });
  await runtime.call('configure', { accounts: [{ id: 'dt', provider: 'dingtalk', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  await new Promise((resolve) => setTimeout(resolve, 0));
  const account = runtime.accounts.get('dt');
  assert.equal(client.options.autoReconnect, false);
  assert.equal(account.status, 'ready');
  client.registered = true;
  runtime.syncAccountStatus(account);
  assert.equal(account.status, 'ready');
  client.registered = false;
  runtime.syncAccountStatus(account);
  assert.equal(account.status, 'ready');
  client.connected = false;
  runtime.syncAccountStatus(account);
  assert.equal(account.status, 'reconnecting');
  await runtime.close();
});

test('钉钉在 WebSocket 尚未连接时保持 connecting，连接后立即 ready', async () => {
  let client;
  class FakeDingTalk extends EventEmitter {
    constructor() { super(); client = this; this.connected = false; this.registered = false; }
    registerCallbackListener() {}
    async connect() { setTimeout(() => { this.connected = true; }, 80); }
    disconnect() { this.connected = false; }
    socketCallBackResponse() {}
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { DWClient: FakeDingTalk } });
  await runtime.call('configure', { accounts: [{ id: 'dt-fast', provider: 'dingtalk', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  try {
    const account = runtime.accounts.get('dt-fast');
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(account.supervisor !== undefined, true);
    await new Promise((resolve) => setTimeout(resolve, 1_150));
    assert.equal(client.connected, true);
    assert.equal(client.registered, false);
    assert.equal(account.status, 'ready');
  } finally {
    await runtime.close();
  }
});

test('钉钉官方扫码只返回应用凭据时查询本应用名称，名称查询拒绝仍保留机器人入口', async (t) => {
  for (const denied of [false, true]) await t.test(denied ? '元信息查询拒绝' : '元信息返回真实应用名称', async () => {
    const events = [];
    const paths = [];
    class FakeDingTalk extends EventEmitter {
      constructor() { super(); this.connected = true; }
      registerCallbackListener() {}
      async connect() {}
      disconnect() { this.connected = false; }
    }
    const runtime = new Runtime({ emit: (event) => events.push(event), sleepImpl: async () => {}, sdk: { DWClient: FakeDingTalk }, fetchImpl: async (url, options) => {
      const path = new URL(url).pathname;
      paths.push(path);
      if (path === '/app/registration/init') return Response.json({ errcode: 0, nonce: 'local-nonce' });
      if (path === '/app/registration/begin') return Response.json({ errcode: 0, device_code: 'local-device', verification_uri_complete: 'https://open-dev.dingtalk.com/openapp/registration/openClaw', expires_in: 7200, interval: 3 });
      if (path === '/app/registration/poll') return Response.json({ errcode: 0, status: 'SUCCESS', client_id: 'local-client', client_secret: 'local-secret' });
      if (path === '/v1.0/oauth2/accessToken') return Response.json({ accessToken: 'local-token', expireIn: 7200 });
      assert.equal(path, '/v1.0/microApp/app/detail');
      assert.equal(options.headers['x-acs-dingtalk-access-token'], 'local-token');
      return denied ? Response.json({ code: 'Forbidden' }, { status: 403 }) : Response.json({ name: '授权创建的应用名称', homepageLink: 'https://example.invalid/homepage' });
    } });
    try {
      await runtime.beginBinding({ provider: 'dingtalk', bindingId: 'dt-name' });
      for (let i = 0; i < 100 && paths.length < 5; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
      const binding = runtime.bindingStatus('dt-name');
      assert.equal(binding.status, 'complete');
      assert.equal(binding.botUrl, 'https://open-dev.dingtalk.com/fe/app#/corp/robot');
      assert.equal(binding.botName, undefined);
      assert.equal(binding.appName, denied ? undefined : '授权创建的应用名称');
      assert.deepEqual(binding.targets, []);
      assert.equal(paths.filter((path) => path === '/v1.0/microApp/app/detail').length, 1);
      assert.equal(JSON.stringify(events).includes('local-secret'), false);
      assert.equal(JSON.stringify(events).includes('local-token'), false);
      assert.equal(JSON.stringify(events).includes('example.invalid'), false);
    } finally { await runtime.close(); }
  });
});

test('官方钉钉 SDK 从本地 Stream 回调发现绑定个人目标', { timeout: 5_000 }, async () => {
  const server = new WebSocketServer({ port: 0, host: '127.0.0.1' });
  await once(server, 'listening');
  class LocalDingTalk extends OfficialDingTalkClient {
    async getEndpoint() { this.dw_url = `ws://127.0.0.1:${server.address().port}`; }
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { DWClient: LocalDingTalk } });
  const binding = { id: 'dt-official', provider: 'dingtalk', status: 'complete', controller: new AbortController(), credentials: { appId: 'local-app', appSecret: 'local-secret' }, targets: [] };
  runtime.bindings.set(binding.id, binding);
  let peer;
  try {
    const accepted = once(server, 'connection');
    await runtime.startProvisionalBinding(binding);
    [peer] = await accepted;
    const account = runtime.accounts.get(binding.id);
    if (!account.client.connected) await once(account.client.socket, 'open');
    runtime.syncAccountStatus(account);
    // 采用 SDK 实际订阅的 topic，验证标准回调信封与 JSON 字符串消息。
    const topic = account.client.config.subscriptions.find((item) => item.type === 'CALLBACK').topic;
    assert.equal(topic, TOPIC_ROBOT);
    const discovered = once(runtime.accountChanges, `account:${binding.id}`);
    peer.send(JSON.stringify({ type: 'CALLBACK', headers: { topic: TOPIC_ROBOT, messageId: 'local-message' }, data: JSON.stringify({ conversationType: '1', senderStaffId: 'staff-1', senderId: '$:LWCP_v1:$opaque', senderNick: '成员' }) }));
    await discovered;
    assert.deepEqual(runtime.bindingStatus(binding.id).targets, [{ id: 'staff-1', kind: 'user', label: '成员' }]);
  } finally {
    await runtime.close();
    peer?.terminate();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('钉钉扫码授权与连接分开，私信缺少员工 ID 时明确说明并在有效回调后恢复', async () => {
  let callback;
  let client;
  class FakeDingTalk extends EventEmitter {
    constructor() { super(); client = this; this.connected = false; }
    registerCallbackListener(_topic, handler) { callback = handler; }
    async connect() {}
    disconnect() { this.connected = false; }
    socketCallBackResponse() {}
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { DWClient: FakeDingTalk } });
  const binding = { id: 'dt-binding', provider: 'dingtalk', status: 'complete', controller: new AbortController(), credentials: { appId: 'id', appSecret: 's' }, targets: [] };
  runtime.bindings.set(binding.id, binding);
  try {
    await runtime.startProvisionalBinding(binding);
    assert.equal(runtime.bindingStatus(binding.id).connectionStatus, 'connecting');
    client.connected = true;
    runtime.syncAccountStatus(runtime.accounts.get(binding.id));
    assert.equal(runtime.bindingStatus(binding.id).connectionStatus, 'ready');
    callback({ headers: { messageId: 'local-message' }, data: JSON.stringify({ conversationType: '1', senderId: '$:LWCP_v1:$opaque', senderNick: '成员' }) });
    const missing = runtime.bindingStatus(binding.id);
    assert.deepEqual(missing.targets, []);
    assert.equal(missing.message, 'dingtalk_missing_staff_id');
    callback({ headers: {}, data: JSON.stringify({ conversationType: '1', senderStaffId: 'staff-1', senderId: '$:LWCP_v1:$opaque', senderNick: '成员' }) });
    const recovered = runtime.bindingStatus(binding.id);
    assert.equal(recovered.message, undefined);
    assert.deepEqual(recovered.targets, [{ id: 'staff-1', kind: 'user', label: '成员' }]);
    client.connected = false;
    runtime.syncAccountStatus(runtime.accounts.get(binding.id));
    assert.equal(runtime.bindingStatus(binding.id).connectionStatus, 'reconnecting');
  } finally { await runtime.close(); }
});

test('钉钉回调优先使用 senderStaffId，且数字 conversationType 识别为群', async () => {
  let callback;
  class FakeDingTalk extends EventEmitter {
    constructor() { super(); this.connected = true; this.registered = true; }
    registerCallbackListener(_topic, handler) { callback = handler; }
    async connect() {}
    disconnect() { this.connected = false; }
    socketCallBackResponse() {}
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { DWClient: FakeDingTalk } });
  await runtime.call('configure', { accounts: [{ id: 'dt-target', provider: 'dingtalk', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  await new Promise((resolve) => setTimeout(resolve, 0));
  runtime.accounts.get('dt-target').targets.push({ id: 'user-opaque', kind: 'user', label: '成员' });
  callback({ headers: {}, data: { conversationType: '1', senderStaffId: 'staff-1', senderId: 'user-opaque', senderNick: '成员' } });
  for (let i = 0; i < 20; i += 1) callback({ headers: {}, data: { conversationType: '1', senderStaffId: 'staff-1', senderId: `opaque-${i}`, senderNick: '成员' } });
  callback({ headers: {}, data: { conversationType: '1', senderId: 'undeliverable-id', senderNick: '成员' } });
  callback({ headers: {}, data: { conversationType: 2, conversationId: 'cid-2', conversationTitle: '群聊' } });
  assert.deepEqual(runtime.accounts.get('dt-target').targets, [
    { id: 'staff-1', kind: 'user', label: '成员' }, { id: 'cid-2', kind: 'chat', label: '群聊' },
  ]);
  assert.equal(runtime.accounts.get('dt-target').credentials.targetAliases['user-opaque'], 'staff-1');
  await runtime.close();
});

test('停用等待中的钉钉 SDK 导入后，不创建迟到连接', async () => {
  const runtime = new Runtime({ emit: () => {} });
  await runtime.call('configure', { accounts: [{ id: 'dt-late', provider: 'dingtalk', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  await runtime.call('configure', { accounts: [] });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(runtime.accounts.has('dt-late'), false);
  await runtime.close();
});

test('官方 SDK 请求在退出、停用和取消临时绑定后释放代理连接', async (t) => {
  class FakeWs { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
  class FakeDispatcher { register() { return this; } }
  for (const [provider, action] of ['dingtalk', 'feishu', 'feishu-groups', 'feishu-ws', 'wecom'].flatMap((provider) => ['退出', '停用', '取消临时绑定'].map((action) => [provider, action]))) {
    await t.test(`${provider} ${action}`, { timeout: 5_000 }, async () => {
      const sockets = new Set();
      const proxy = createServer();
      proxy.on('connection', (socket) => {
        sockets.add(socket);
        socket.on('error', () => {});
        socket.on('end', () => socket.end());
        socket.on('close', () => { sockets.delete(socket); if (sockets.size === 0) proxy.emit('drained'); });
      });
      const connected = once(proxy, 'connect', { signal: AbortSignal.timeout(2_000) });
      proxy.listen(0, '127.0.0.1');
      await once(proxy, 'listening');
      const runtime = new Runtime({ emit: () => {}, sdk: ['feishu', 'feishu-groups'].includes(provider) ? { FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } : {} });
      const apply = runtime.applyRequestSignal.bind(runtime);
      let endpointRequested = false;
      runtime.applyRequestSignal = (config) => {
        const configured = apply(config);
        if (provider === 'feishu-ws' && config.url.endsWith('/callback/ws/endpoint')) { endpointRequested = true; assert.ok(configured.signal); }
        return configured;
      };
      if (provider === 'feishu-ws') {
        runtime.sdk.FeishuClient = class { async request() { return { code: 0, bot: { app_name: 'fixture' } }; } };
        const OfficialWs = runtime.sdk.FeishuWSClient;
        runtime.sdk.FeishuWSClient = class extends OfficialWs {
          constructor(options) { assert.equal(options.httpInstance, runtime.httpInstance); super(options); }
        };
      }
      if (provider === 'feishu-groups') {
        const OfficialClient = runtime.sdk.FeishuClient;
        runtime.sdk.FeishuClient = class extends OfficialClient {
          constructor(options) {
            options.httpInstance.defaults.adapter = async (config) => {
              if (config.url.endsWith('/open-apis/im/v1/chats')) return axios.getAdapter('http')(config);
              const data = config.url.includes('tenant_access_token') ? { code: 0, tenant_access_token: 'fixture', expire: 7200 } : { code: 0, bot: { app_name: 'fixture' } };
              return { data, status: 200, statusText: 'OK', headers: {}, config };
            };
            super(options);
          }
        };
      }
      const account = { id: 'sdk-cancel', provider: provider.startsWith('feishu') ? 'feishu' : provider, credentials: provider === 'wecom' ? { botId: 'fixture', secret: 'fixture' } : { appId: 'cli_0000000000000000', appSecret: 'fixture' }, targets: [], enabled: true };
      let pendingSend;
      let pendingGroups;
      let ws;
      try {
        await runtime.configure({ network: 'system_proxy', proxyUrl: `http://127.0.0.1:${proxy.address().port}`, accounts: action === '取消临时绑定' ? [] : [account] });
        if (action === '取消临时绑定') {
          const binding = { id: account.id, provider: account.provider, credentials: account.credentials, targets: [], status: 'complete', controller: new AbortController() };
          runtime.bindings.set(binding.id, binding);
          void runtime.startProvisionalBinding(binding);
        }
        if (provider === 'feishu') pendingSend = runtime.send({ accountId: account.id, target: { id: 'fixture-chat', kind: 'chat' }, text: 'fixture' });
        if (provider === 'feishu-groups') {
          for (let i = 0; i < 50 && runtime.accounts.get(account.id).status !== 'ready'; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
          pendingGroups = (action === '取消临时绑定' ? runtime.detectBindingGroups(account.id) : runtime.detectGroups(account.id)).then(() => 'completed', () => 'cancelled');
        }
        const [request, socket] = await connected;
        assert.equal(request.url, provider === 'dingtalk' ? 'api.dingtalk.com:443' : provider === 'wecom' ? 'openws.work.weixin.qq.com:443' : 'open.feishu.cn:443');
        if (provider === 'feishu-ws') assert.equal(endpointRequested, true);
        ws = runtime.accounts.get(account.id).ws;
        const socketClosed = once(proxy, 'drained', { signal: AbortSignal.timeout(1_000) });
        if (action === '退出') await runtime.close();
        else if (action === '停用') await runtime.configure({ network: 'system_proxy', proxyUrl: runtime.proxyUrl, accounts: [{ ...account, enabled: false }] });
        else assert.deepEqual(runtime.cancelBinding(account.id), { cancelled: true });
        await socketClosed;
        if (pendingSend) assert.equal((await pendingSend).outcome, 'unknown');
        if (pendingGroups) assert.equal(await pendingGroups, 'cancelled');
        assert.equal(sockets.size, 0);
      } finally {
        await runtime.close();
        // Node 原版 SDK 未应用构建期握手补丁，收尾其迟到重连计时器。
        ws?.close({ force: true });
        for (const socket of sockets) socket.destroy();
        await new Promise((resolve) => proxy.close(resolve));
      }
    });
  }
});

test('飞书 has_more 缺少 page_token 时拒绝返回不完整群列表', async () => {
  class FakeClient { constructor() { this.im = { v1: { chat: { list: async () => ({ code: 0, data: { has_more: true, items: [] } }) } } }; } }
  class FakeWs { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
  class FakeDispatcher { register() { return this; } }
  const runtime = new Runtime({ emit: () => {}, sdk: { FeishuClient: FakeClient, FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  await runtime.call('configure', { accounts: [{ id: 'f-page', provider: 'feishu', credentials: { appId: 'id', appSecret: 's' }, targets: [], enabled: true }] });
  await new Promise((resolve) => setTimeout(resolve, 0));
  await assert.rejects(runtime.call('detect_groups', { accountId: 'f-page' }), /分页响应不完整/);
  await runtime.close();
});

test('官方飞书 SDK 通过注入 Axios adapter 保持响应解包契约', async () => {
  const requests = [];
  let holdSend = false;
  let pendingSignal;
  let pendingRequestResolve;
  const pendingRequest = new Promise((resolve) => { pendingRequestResolve = resolve; });
  let axiosAborted = false;
  class FakeWs { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
  class FakeDispatcher { register() { return this; } }
  const runtime = new Runtime({ emit: () => {}, sdk: { FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
  const adapter = async (config) => {
    requests.push(config);
    if (holdSend && config.url.endsWith('/open-apis/im/v1/messages')) return new Promise((_resolve, reject) => {
      pendingSignal = config.signal;
      pendingRequestResolve();
      config.signal.addEventListener('abort', () => { axiosAborted = true; reject(new DOMException('aborted', 'AbortError')); }, { once: true });
    });
    const data = config.url.includes('tenant_access_token')
      ? { code: 0, tenant_access_token: 'token', expire: 7200 }
      : config.url.endsWith('/open-apis/im/v1/chats')
        ? { code: 0, data: { has_more: false, items: [{ chat_id: 'oc_group', name: '群组', chat_mode: 'group' }] } }
        : config.url.includes('/bot/v3/info') ? { code: 0, bot: { app_name: 'fixture' } }
        : { code: 0, data: { message_id: 'om_sent' } };
    return { data, status: 200, statusText: 'OK', headers: {}, config };
  };
  const OfficialClient = runtime.sdk.FeishuClient;
  runtime.sdk.FeishuClient = class extends OfficialClient {
    constructor(options) { options.httpInstance.defaults.adapter = adapter; super(options); }
  };
  await runtime.call('configure', { accounts: [{ id: 'official-feishu', provider: 'feishu', credentials: { appId: 'cli_test', appSecret: 'secret' }, targets: [], enabled: true }] });
  for (let i = 0; i < 100 && runtime.accounts.get('official-feishu').status === 'connecting'; i += 1) await new Promise((resolve) => setTimeout(resolve, 1));
  assert.deepEqual(await runtime.call('detect_groups', { accountId: 'official-feishu' }), { targets: [{ id: 'oc_group', kind: 'chat', label: '群组' }] });
  assert.deepEqual(await runtime.call('send', { accountId: 'official-feishu', target: { id: 'oc_group', kind: 'chat' }, text: '库存更新' }), { outcome: 'accepted', message: '平台已接受' });
  assert.equal(requests.some((request) => request.url.endsWith('/open-apis/im/v1/chats')), true);
  assert.equal(requests.some((request) => request.url.endsWith('/open-apis/im/v1/messages')), true);
  holdSend = true;
  const pendingSend = runtime.call('send', { accountId: 'official-feishu', target: { id: 'oc_group', kind: 'chat' }, text: '未完成发送' });
  await pendingRequest;
  assert.ok(pendingSignal);
  await runtime.close();
  assert.equal((await pendingSend).outcome, 'unknown');
  assert.equal(axiosAborted, true);
});

test('非飞书渠道检测群聊返回运行时已识别的 chat targets', async () => {
  const runtime = new Runtime({ emit: () => {} });
  runtime.accounts.set('wecom-detect', { id: 'wecom-detect', provider: 'wecom', status: 'ready', targets: [
    { id: 'group-1', kind: 'chat', label: '已识别群' }, { id: 'user-1', kind: 'user', label: '联系人' },
  ] });
  assert.deepEqual(await runtime.call('detect_groups', { accountId: 'wecom-detect' }), { targets: [{ id: 'group-1', kind: 'chat', label: '已识别群' }] });
  await runtime.close();
});

test('微信 iLink 业务层凭据失效停止轮询，其他拒绝响应进入有间隔的重试', async () => {
  let fetches = 0;
  let sleeps = 0;
  const runtime = new Runtime({
    emit: () => {},
    fetchImpl: async () => { fetches += 1; return Response.json({ ret: -14 }); },
  });
  await runtime.call('configure', { accounts: [{ id: 'wx-auth', provider: 'weixin', credentials: { botToken: 't' }, targets: [], enabled: true }] });
  for (let i = 0; i < 50 && runtime.accounts.get('wx-auth').status !== 'auth_required'; i += 1) await new Promise((resolve) => setTimeout(resolve, 1));
  assert.equal(fetches, 1);
  assert.equal(runtime.accounts.get('wx-auth').status, 'auth_required');
  await runtime.close();

  const retryRuntime = new Runtime({
    emit: () => {},
    fetchImpl: async (input) => { if (String(input).includes('notifystart') || String(input).includes('notifystop')) return Response.json({ ret: 0 }); fetches += 1; return Response.json({ errcode: 7 }); },
    sleepImpl: (_ms, signal) => new Promise((resolve, reject) => {
      sleeps += 1;
      signal.addEventListener('abort', () => reject(signal.reason), { once: true });
    }),
  });
  await retryRuntime.call('configure', { accounts: [{ id: 'wx-reject', provider: 'weixin', credentials: { botToken: 't' }, targets: [], enabled: true }] });
  for (let i = 0; i < 50 && sleeps === 0; i += 1) await new Promise((resolve) => setTimeout(resolve, 1));
  assert.equal(sleeps, 1);
  await retryRuntime.close();
});

test('direct 网络模式显式清空 Bun fetch 代理', async () => {
  const originalBun = globalThis.Bun;
  if (!originalBun) globalThis.Bun = {};
  const calls = [];
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async (_url, options) => { calls.push(options); return Response.json({ ok: true }); } });
  try {
    await runtime.call('configure', { network: 'direct', accounts: [] });
    await runtime.fetch('https://example.test/');
    assert.equal(calls[0].proxy, '');
  } finally {
    await runtime.close();
    if (!originalBun) globalThis.Bun = originalBun;
  }
});

test('configure 不替换进程级 HTTPS agent', async () => {
  const originalAgent = https.globalAgent;
  const runtime = new Runtime({ emit: () => {} });
  try {
    await runtime.call('configure', { network: 'direct', accounts: [] });
    assert.equal(https.globalAgent, originalAgent);
    await runtime.call('configure', { network: 'system_proxy', proxyUrl: 'http://127.0.0.1:8080', accounts: [] });
    assert.equal(https.globalAgent, originalAgent);
  } finally {
    await runtime.close();
  }
});

test('企微 SDK 关闭连接会拒绝未决 WebSocket 回执并清空 socket', async () => {
  const server = new WebSocketServer({ port: 0, host: '127.0.0.1' });
  await new Promise((resolve) => server.once('listening', resolve));
  let pendingFrameResolve;
  const pendingFrame = new Promise((resolve) => { pendingFrameResolve = resolve; });
  const sentFrames = [];
  let closedResolve;
  const socketClosed = new Promise((resolve) => { closedResolve = resolve; });
  server.once('connection', (socket) => {
    socket.once('close', closedResolve);
    socket.on('message', (raw) => {
      const frame = JSON.parse(String(raw));
      if (frame.cmd === 'aibot_subscribe') socket.send(JSON.stringify({ headers: { req_id: frame.headers.req_id }, errcode: 0 }));
      else {
        sentFrames.push(frame);
        if (sentFrames.length === 1) socket.send(JSON.stringify({ headers: { req_id: frame.headers.req_id }, errcode: 0 }));
        else pendingFrameResolve(frame);
      }
    });
  });
  class LocalWecomClient extends OfficialWecomWSClient {
    constructor(options) { super({ ...options, wsUrl: `ws://127.0.0.1:${server.address().port}`, maxReconnectAttempts: -1, heartbeatInterval: 60_000 }); }
  }
  const runtime = new Runtime({ emit: () => {}, sdk: { WecomWSClient: LocalWecomClient } });
  await runtime.call('configure', { accounts: [{ id: 'local-wecom', provider: 'wecom', credentials: { botId: 'bot', secret: 'secret' }, targets: [], enabled: true }] });
  for (let i = 0; i < 100 && runtime.accounts.get('local-wecom').status !== 'ready'; i += 1) await new Promise((resolve) => setTimeout(resolve, 5));
  const params = { accountId: 'local-wecom', target: { id: 'chat', kind: 'chat' }, text: '**hello**' };
  assert.deepEqual(await runtime.call('send', params), { outcome: 'accepted', message: '平台已接受' });
  assert.deepEqual(sentFrames[0].body, { chatid: 'chat', msgtype: 'markdown', markdown: { content: '**hello**' } });
  const pending = runtime.call('send', params);
  await pendingFrame;
  await runtime.close();
  assert.equal((await pending).outcome, 'unknown');
  await socketClosed;
  assert.equal(server.clients.size, 0);
  await new Promise((resolve) => server.close(resolve));
});
