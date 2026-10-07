import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../src/main.mjs';
import { createWeixinTextApi } from '../src/weixin-text-api.mjs';

const tick = () => new Promise((resolve) => setImmediate(resolve));
async function until(check) {
  for (let i = 0; i < 50 && !check(); i += 1) await tick();
  assert.ok(check(), '预期的运行时事件应已完成');
}
const abortOnly = (signal) => new Promise((_resolve, reject) => {
  if (signal.aborted) reject(signal.reason);
  else signal.addEventListener('abort', () => reject(signal.reason), { once: true });
});

test('微信获取二维码的 JSON POST 使用公共鉴权类型与 UIN，状态 GET 使用应用头', async () => {
  const calls = [];
  const api = createWeixinTextApi(async (_input, options) => {
    calls.push(options);
    return Response.json(options.method === 'POST' ? { qrcode: 'fixture-qr', qrcode_img_content: 'https://ilinkai.weixin.qq.com/fixture-qr' } : { status: 'wait' });
  });
  await api.beginLogin();
  await api.pollLogin({ qrcode: 'fixture-qr' });
  const post = new Headers(calls[0].headers);
  assert.equal(post.get('content-type'), 'application/json');
  assert.equal(post.get('AuthorizationType'), 'ilink_bot_token');
  assert.match(Buffer.from(post.get('X-WECHAT-UIN'), 'base64').toString(), /^\d+$/);
  assert.equal(post.get('Authorization'), null);
  assert.deepEqual(JSON.parse(calls[0].body), { local_token_list: [] });
  const get = new Headers(calls[1].headers);
  assert.equal(get.get('iLink-App-Id'), 'bot');
  assert.equal(get.get('AuthorizationType'), null);
  assert.equal(get.get('X-WECHAT-UIN'), null);
});

test('微信首次扫码直接测试，收到消息后使用对应会话上下文', async (t) => {
  const events = [];
  const sent = [];
  let deliver;
  let polls = 0;
  const runtime = new Runtime({
    emit: (event) => events.push(event),
    fetchImpl: async (input, options) => {
      const path = new URL(input).pathname;
      if (path.endsWith('get_bot_qrcode')) return Response.json({ qrcode: 'fixture-qr', qrcode_img_content: 'https://ilinkai.weixin.qq.com/fixture-qr' });
      if (path.endsWith('get_qrcode_status')) return Response.json({ status: 'confirmed', bot_token: 'fixture-token', ilink_bot_id: 'fixture-bot', ilink_user_id: 'fixture-owner' });
      if (path.endsWith('notifystart') || path.endsWith('notifystop')) return Response.json({ ret: 0 });
      if (path.endsWith('sendmessage')) { sent.push(JSON.parse(options.body)); return Response.json({ ret: 0 }); }
      polls += 1;
      if (polls === 1) return Response.json({ ret: 0, get_updates_buf: 'fixture-cursor', msgs: [] });
      if (polls === 2) return new Promise((resolve) => { deliver = (message) => resolve(Response.json({ ret: 0, msgs: [message] })); });
      return abortOnly(options.signal);
    },
  });
  t.after(() => runtime.close());
  await runtime.beginBinding({ provider: 'weixin', bindingId: 'first-contact' });
  await until(() => deliver && runtime.bindings.get('first-contact').status === 'complete');
  const bound = runtime.bindingStatus('first-contact');
  assert.equal(bound.connectionStatus, 'ready');
  assert.equal(bound.message, undefined);
  const params = { accountId: 'first-contact', target: { id: 'fixture-owner', kind: 'user' }, text: '本地测试' };
  const missing = await runtime.send(params);
  assert.equal(missing.outcome, 'accepted');
  assert.equal(sent.length, 1);
  assert.equal(sent[0].msg.context_token, undefined);

  deliver({ from_user_id: 'fixture-owner', to_user_id: 'fixture-bot', message_type: 1, context_token: 'fixture-context' });
  await until(() => runtime.accounts.get('first-contact').credentials.contextTokens['fixture-owner']);
  assert.equal(runtime.bindingStatus('first-contact').message, undefined);
  assert.ok(events.some((event) => event.event === 'account' && event.data.status === 'ready'));
  assert.equal(JSON.stringify(events.filter((event) => event.event !== 'credentials')).includes('fixture-context'), false);
  assert.deepEqual(await runtime.send(params), { outcome: 'accepted', message: '平台已接受' });
  assert.equal(sent.length, 3);
  assert.equal(sent[1].msg.item_list[0].text_item.text, '收到，配对成功！');
  assert.equal(sent[1].msg.to_user_id, 'fixture-owner');
  assert.equal(sent[1].msg.context_token, 'fixture-context');
  assert.equal(sent[0].msg.message_type, 2);
  assert.equal(sent[0].msg.message_state, 2);
  assert.deepEqual(sent[0].msg.item_list, [{ type: 1, text_item: { text: '本地测试' } }]);
});

test('微信会话按用户隔离，单值旧令牌不发送给当前或其他用户', async (t) => {
  const sent = [];
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async (_input, options) => { sent.push(JSON.parse(options.body)); return Response.json({ ret: 0 }); } });
  t.after(() => runtime.close());
  const account = { id: 'isolated', provider: 'weixin', enabled: true, status: 'ready', targets: [], credentials: { token: 'fixture-token', userId: 'owner', contextToken: 'unscoped-context', contextTokens: { other: 'other-context' } } };
  runtime.accounts.set(account.id, account);
  for (const id of ['owner', 'new-user']) {
    assert.equal((await runtime.send({ accountId: account.id, target: { id, kind: 'user' }, text: '本地测试' })).outcome, 'accepted');
  }
  assert.equal(sent.length, 2);
  assert.equal(sent[0].msg.context_token, undefined);
  assert.equal(sent[1].msg.context_token, undefined);
  assert.equal((await runtime.send({ accountId: account.id, target: { id: 'other', kind: 'user' }, text: '本地测试' })).outcome, 'accepted');
  assert.equal(sent[2].msg.context_token, 'other-context');
});

test('微信真实平台拒绝保留非零 ret/errcode，不推定 -2 原因或重试', async (t) => {
  let sent = 0;
  let response;
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async () => { sent += 1; return Response.json(response); } });
  t.after(() => runtime.close());
  const account = { id: 'rejected', provider: 'weixin', enabled: true, status: 'ready', targets: [], credentials: { token: 'fixture-token', contextTokens: { owner: 'fixture-context' } } };
  runtime.accounts.set(account.id, account);
  for (const value of [{ ret: -2 }, { ret: 0, errcode: -2 }, { ret: '0', errcode: '-2' }]) {
    response = value;
    const result = await runtime.send({ accountId: account.id, target: { id: 'owner', kind: 'user' }, text: '本地测试' });
    assert.deepEqual(result, { outcome: 'failed', message: '微信 iLink 拒绝发送（-2）', retryable: false });
  }
  assert.equal(sent, 3);
});

test('微信旧连接结束后的迟到上下文不会污染重新扫码的新账号', async (t) => {
  let releaseOld;
  const runtime = new Runtime({ emit: () => {}, fetchImpl: async (_input, options) => {
    if (String(_input).includes('notifystart') || String(_input).includes('notifystop')) return Response.json({ ret: 0 });
    if (options.headers.Authorization === 'Bearer old-token') return new Promise((resolve) => { releaseOld = () => resolve(Response.json({ ret: 0, msgs: [{ from_user_id: 'owner', context_token: 'old-context' }] })); });
    return abortOnly(options.signal);
  } });
  t.after(() => runtime.close());
  const config = (botToken) => ({ accounts: [{ id: 'rebound', provider: 'weixin', enabled: true, targets: [{ id: 'owner', kind: 'user' }], credentials: { botToken, userId: 'owner', contextTokens: {} } }] });
  await runtime.configure(config('old-token'));
  await until(() => releaseOld);
  const old = runtime.accounts.get('rebound');
  await runtime.configure(config('new-token'));
  releaseOld();
  await tick();
  assert.deepEqual(old.credentials.contextTokens, {});
  assert.deepEqual(runtime.accounts.get('rebound').credentials.contextTokens, {});
  await until(() => runtime.accounts.get('rebound').status === 'ready');
});

test('微信首次发送被平台拒绝时提示建立私聊，发送仍由平台回执判定', async (t) => {
  let contacted = false;
  const sent = [];
  const runtime = new Runtime({ emit() {}, fetchImpl: async (_input, options) => {
    sent.push(JSON.parse(options.body));
    return Response.json(contacted ? { ret: 0 } : { ret: -2, errmsg: 'prepare failed' });
  } });
  t.after(() => runtime.close());
  const account = { id: 'new-contact', provider: 'weixin', enabled: true, status: 'ready', targets: [], credentials: { token: 'fixture-token', contextTokens: {} } };
  runtime.accounts.set(account.id, account);
  const params = { accountId: account.id, target: { id: 'owner', kind: 'user' }, text: '本地测试' };
  assert.deepEqual(await runtime.send(params), {
    outcome: 'failed', retryable: false,
    message: '微信 iLink 拒绝发送（-2）。请向微信机器人发送一条私信，再点击测试。',
  });
  assert.equal(account.status, 'ready');
  contacted = true;
  assert.deepEqual(await runtime.send(params), { outcome: 'accepted', message: '平台已接受' });
  assert.equal(sent.length, 2);
  assert.equal(sent[0].msg.context_token, undefined);
  assert.equal(sent[1].msg.context_token, undefined);
});
