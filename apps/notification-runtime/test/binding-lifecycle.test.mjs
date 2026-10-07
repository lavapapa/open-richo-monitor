import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../src/main.mjs';

const tick = () => new Promise((resolve) => setImmediate(resolve));
async function until(check) {
  for (let i = 0; i < 100 && !check(); i += 1) await tick();
  assert.ok(check());
}
const pending = (signal) => new Promise((_resolve, reject) => {
  if (signal.aborted) reject(signal.reason);
  else signal.addEventListener('abort', () => reject(signal.reason), { once: true });
});

test('微信启动成功后首次无上下文可投递，停用与退出发送停止通知', async (t) => {
  const calls = [];
  let releaseStart;
  const runtime = new Runtime({ emit() {}, fetchImpl: async (input, options) => {
    const path = new URL(input).pathname;
    calls.push(path);
    if (path.endsWith('notifystart')) return new Promise((resolve) => { releaseStart = () => resolve(Response.json({ ret: 0 })); });
    if (path.endsWith('getupdates')) return pending(options.signal);
    return Response.json({ ret: 0 });
  } });
  t.after(() => runtime.close());
  const config = { accounts: [{ id: 'wx', provider: 'weixin', credentials: { botToken: 'fixture', userId: 'owner' }, targets: [], enabled: true }] };
  runtime.configure(config);
  await until(() => releaseStart);
  assert.equal(runtime.accounts.get('wx').status, 'connecting');
  assert.equal(calls.some((path) => path.endsWith('getupdates')), false);
  releaseStart();
  await until(() => runtime.accounts.get('wx').status === 'ready');
  assert.equal((await runtime.send({ accountId: 'wx', target: { id: 'owner', kind: 'user' }, text: '首次测试' })).outcome, 'accepted');
  await runtime.close();
  assert.equal(calls.filter((path) => path.endsWith('notifystop')).length, 1);
});

test('微信扫码保存复用在线连接，重连等待停止回执，每轮停止一次', async (t) => {
  const calls = [];
  let releaseStop;
  const runtime = new Runtime({ emit() {}, fetchImpl: async (input, options) => {
    const path = new URL(input).pathname;
    calls.push(path);
    if (path.endsWith('getupdates')) return pending(options.signal);
    if (path.endsWith('notifystop') && !releaseStop) return new Promise((resolve) => { releaseStop = () => resolve(Response.json({ ret: 0 })); });
    return Response.json({ ret: 0 });
  } });
  t.after(() => runtime.close());
  const credentials = { botToken: 'fixture', userId: 'owner' };
  const binding = { id: 'pending', provider: 'weixin', status: 'complete', credentials, targets: [], controller: new AbortController() };
  runtime.bindings.set(binding.id, binding);
  await runtime.startProvisionalBinding(binding);
  const account = runtime.accounts.get(binding.id);
  runtime.configure({ accounts: [{ id: 'saved', provider: 'weixin', credentials: { ...credentials }, enabled: true, targets: [] }] });
  assert.equal(runtime.accounts.get('saved'), account);
  assert.equal(calls.filter((path) => path.endsWith('notifystart')).length, 1);
  runtime.stopAccount(account);
  const restarting = runtime.startWeixin(account);
  await until(() => releaseStop);
  assert.equal(calls.filter((path) => path.endsWith('notifystart')).length, 1);
  releaseStop();
  await restarting;
  await runtime.close();
  assert.equal(calls.filter((path) => path.endsWith('notifystart')).length, 2);
  assert.equal(calls.filter((path) => path.endsWith('notifystop')).length, 2);
});

test('钉钉回执中的无效和限流接收人均不可标为发送成功', async (t) => {
  let result;
  const runtime = new Runtime({ emit() {}, fetchImpl: async (input) => Response.json(String(input).includes('accessToken') ? { accessToken: 'fixture' } : result) });
  t.after(() => runtime.close());
  runtime.accounts.set('ding', { id: 'ding', provider: 'dingtalk', enabled: true, status: 'ready', targets: [], credentials: { appId: 'app', appSecret: 'secret' } });
  for (const field of ['invalidStaffIdList', 'flowControlledStaffIdList']) {
    result = { processQueryKey: 'query', [field]: ['owner'] };
    const outcome = await runtime.send({ accountId: 'ding', target: { id: 'owner', kind: 'user' }, text: '测试' });
    assert.equal(outcome.outcome, 'failed');
    assert.equal(outcome.retryable, field === 'flowControlledStaffIdList');
  }
});

test('飞书资料业务认证错误进入重新授权，停止网络重试', async (t) => {
  class Client { async request() { return { code: 10015 }; } }
  const runtime = new Runtime({ emit() {}, sdk: { FeishuClient: Client } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [{ id: 'lark-auth', provider: 'feishu', enabled: true, targets: [], credentials: { appId: 'app', appSecret: 'secret', domain: 'lark' } }] });
  await until(() => runtime.accounts.get('lark-auth').status === 'auth_required');
  assert.equal(runtime.accounts.get('lark-auth').supervisor, undefined);
});

test('微信验证码分支等待输入，再携带配对码完成扫码', async (t) => {
  let verified = false;
  const runtime = new Runtime({ emit() {}, fetchImpl: async (input, options) => {
    const url = new URL(input);
    if (url.pathname.endsWith('get_bot_qrcode')) return Response.json({ qrcode: 'qr', qrcode_img_content: 'https://ilinkai.weixin.qq.com/qr' });
    if (url.pathname.endsWith('get_qrcode_status')) {
      if (url.searchParams.get('verify_code') !== '123456') return Response.json({ status: 'need_verifycode' });
      verified = true;
      return Response.json({ status: 'confirmed', bot_token: 'fixture', ilink_bot_id: 'bot', ilink_user_id: 'owner' });
    }
    if (url.pathname.endsWith('getupdates')) return pending(options.signal);
    return Response.json({ ret: 0 });
  } });
  t.after(() => runtime.close());
  await runtime.beginBinding({ provider: 'weixin', bindingId: 'verify' });
  await until(() => runtime.bindingStatus('verify').status === 'needs_verification');
  await runtime.call('submit_binding_verification', { bindingId: 'verify', code: '123456' });
  await until(() => runtime.bindingStatus('verify').connectionStatus === 'ready');
  assert.equal(verified, true);
  assert.deepEqual(runtime.bindingStatus('verify').targets, [{ id: 'owner', kind: 'user', label: '绑定账号' }]);
});

test('飞书官方 SDK 的令牌业务认证错误保留重新授权语义', async (t) => {
  const runtime = new Runtime({ emit() {} });
  t.after(() => runtime.close());
  const OfficialClient = runtime.sdk.FeishuClient;
  runtime.sdk.FeishuClient = class extends OfficialClient {
    constructor(options) {
      options.httpInstance.defaults.adapter = async (config) => ({
        data: { code: 10015, msg: 'app secret incorrect' },
        status: 200, statusText: 'OK', headers: {}, config,
      });
      super(options);
    }
  };
  runtime.configure({ accounts: [{ id: 'official-auth', provider: 'feishu', enabled: true, targets: [], credentials: { appId: 'app', appSecret: 'secret' } }] });
  await until(() => runtime.accounts.get('official-auth').status === 'auth_required');
  assert.equal(runtime.accounts.get('official-auth').supervisor, undefined);
});

test('飞书断网恢复时发现凭据失效，停止重试并要求重新授权', async (t) => {
  let authenticationFailed = false;
  class Client {
    async request() {
      if (!authenticationFailed) throw new Error('offline');
      return { code: 10015 };
    }
  }
  const runtime = new Runtime({ emit() {}, sdk: { FeishuClient: Client } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [{ id: 'retry-auth', provider: 'feishu', enabled: true, targets: [], credentials: { appId: 'app', appSecret: 'secret' } }] });
  await until(() => runtime.accounts.get('retry-auth').status === 'failed');
  authenticationFailed = true;
  const account = runtime.accounts.get('retry-auth');
  await runtime.restartAccount(account);
  assert.equal(account.status, 'auth_required');
  assert.equal(account.supervisor, undefined);
});

test('飞书扫码注册使用明确模板，取得机器人资料后保存 Lark 服务域名', async (t) => {
  let options;
  const clientOptions = [];
  class Client {
    constructor(o) { clientOptions.push(o); }
    async request() { return { code: 0, bot: { app_name: '库存助手', open_id: 'ou_bot', activate_status: 2 } }; }
  }
  class WS { constructor(o) { this.o = o; } start() { this.o.onReady(); } close() {} }
  class Dispatcher { register() { return this; } }
  const runtime = new Runtime({ emit() {}, register: async (o) => {
    options = o;
    return { client_id: 'app', client_secret: 'secret', user_info: { open_id: 'ou_owner', tenant_brand: 'lark' } };
  }, sdk: { FeishuClient: Client, FeishuWSClient: WS, FeishuEventDispatcher: Dispatcher } });
  t.after(() => runtime.close());
  runtime.configure({ accounts: [] });
  await runtime.beginBinding({ provider: 'feishu', bindingId: 'lark' });
  await until(() => runtime.bindingStatus('lark').connectionStatus === 'ready');
  assert.equal(options.addons.preset, false);
  assert.equal(runtime.bindings.get('lark').credentials.domain, 'lark');
  assert.equal(clientOptions[0].domain, 'https://open.larksuite.com');
  assert.equal(runtime.bindingStatus('lark').botName, '库存助手');
});

test('企微扫码按运行平台选择参数', async (t) => {
  let platform;
  const runtime = new Runtime({ emit() {}, fetchImpl: async (input, options) => {
    const url = new URL(input);
    if (url.pathname.endsWith('generate')) {
      platform = url.searchParams.get('plat');
      return Response.json({ data: { scode: 'qr', auth_url: 'https://work.weixin.qq.com/qr' } });
    }
    return pending(options.signal);
  } });
  t.after(() => runtime.close());
  await runtime.beginBinding({ provider: 'wecom', bindingId: 'plat' });
  await until(() => platform);
  assert.equal(platform, process.platform === 'win32' ? '2' : process.platform === 'linux' ? '3' : '1');
});
