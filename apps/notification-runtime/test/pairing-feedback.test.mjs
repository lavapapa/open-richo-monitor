import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../src/main.mjs';
import { EventEmitter } from 'node:events';

const tick = () => new Promise(resolve => setImmediate(resolve));

test('四平台配对消息立即确认，同一会话重复消息不会反复回复，保存后停止配对回复', async t => {
  for (const provider of ['weixin', 'feishu', 'wecom', 'dingtalk']) {
    const runtime = new Runtime({ emit() {} });
    t.after(() => runtime.close());
    const binding = { id: provider, provider, status: 'complete', targets: [], credentials: { userId: 'owner', userOpenId: 'owner' }, controller: new AbortController() };
    const account = { id: provider, provider, enabled: true, status: 'ready', targets: [], credentials: binding.credentials, provisionalBinding: binding };
    runtime.bindings.set(binding.id, binding);
    runtime.accounts.set(account.id, account);
    const sent = [];
    runtime.send = async params => { sent.push(params); return { outcome: 'accepted', message: '平台已接受' }; };
    const owner = { id: 'owner', kind: 'user', label: '创建人' };
    const group = { id: 'group', kind: 'chat', label: '测试群' };
    runtime.confirmPairing(account, owner);
    runtime.confirmPairing(account, owner);
    runtime.confirmPairing(account, group);
    await tick();
    assert.equal(sent.length, 2);
    assert.ok(sent.every(item => item.text === '收到，配对成功！'));
    assert.equal(runtime.bindingStatus(binding.id).privateMessageReceived, true);
    assert.equal(runtime.bindingStatus(binding.id).pairingReplies.length, 2);
    account.provisionalBinding = undefined;
    runtime.confirmPairing(account, { id: 'new-group', kind: 'chat' });
    assert.equal(sent.length, 2);
  }
});

test('微信其他人的消息不确认创建人状态，失败回执明确呈现且不自动重发', async t => {
  const runtime = new Runtime({ emit() {} });
  t.after(() => runtime.close());
  const binding = { id: 'wx', provider: 'weixin', status: 'complete', targets: [], credentials: { userId: 'owner' }, controller: new AbortController() };
  const account = { id: 'wx', provider: 'weixin', enabled: true, status: 'ready', targets: [], credentials: binding.credentials, provisionalBinding: binding };
  runtime.accounts.set('wx', account);
  runtime.bindings.set('wx', binding);
  let attempts = 0;
  runtime.send = async () => { attempts++; return { outcome: 'failed', message: '平台拒绝发送' }; };
  runtime.confirmPairing(account, { id: 'other', kind: 'user' });
  await tick();
  assert.notEqual(runtime.bindingStatus('wx').privateMessageReceived, true);
  runtime.confirmPairing(account, { id: 'owner', kind: 'user' });
  await tick();
  runtime.confirmPairing(account, { id: 'owner', kind: 'user' });
  assert.equal(attempts, 2);
  assert.equal(runtime.bindingStatus('wx').pairingReplies[1].outcome, 'failed');
});

test('飞书、企微和钉钉真实回调接入配对确认，进入聊天和群目录发现不冒充收到消息', async t => {
  class Wecom extends EventEmitter { connect() { this.emit('authenticated'); } disconnect() {} }
  class Ding extends EventEmitter {
    connected = true;
    registerCallbackListener(_topic, callback) { this.callback = callback; }
    socketCallBackResponse() {}
    async connect() {}
    disconnect() {}
  }
  class WS { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} }
  class Dispatcher { register(handlers) { this.handlers = handlers; return this; } }
  for (const provider of ['feishu', 'wecom', 'dingtalk']) {
    const runtime = new Runtime({ emit() {}, sdk: { WecomWSClient: Wecom, DWClient: Ding, FeishuClient: class {}, FeishuWSClient: WS, FeishuEventDispatcher: Dispatcher } });
    t.after(() => runtime.close());
    const binding = { id: provider, provider, status: 'complete', targets: [], credentials: { appId: 'app', appSecret: 'secret', botId: 'bot', secret: 'secret', userOpenId: 'owner' }, controller: new AbortController() };
    runtime.bindings.set(provider, binding);
    const sent = [];
    runtime.send = async params => { sent.push(params); return { outcome: 'accepted', message: '平台已接受' }; };
    await runtime.startProvisionalBinding(binding);
    const account = runtime.accounts.get(provider);
    runtime.discoverTarget(account, { id: 'directory-group', kind: 'chat', label: '目录群' });
    if (provider === 'feishu') {
      account.dispatcher.handlers['im.chat.access_event.bot_p2p_chat_entered_v1']({ operator_id: { open_id: 'owner' }, chat_id: 'private' });
      assert.equal(sent.length, 0);
      const receive = account.dispatcher.handlers['im.message.receive_v1'];
      receive({ sender: { sender_id: { open_id: 'owner' } }, message: { chat_type: 'p2p', chat_id: 'private' } });
      receive({ message: { chat_type: 'group', chat_id: 'group' } });
      receive({ message: { chat_type: 'group', chat_id: 'group' } });
    } else if (provider === 'wecom') {
      account.client.emit('event.enter_chat', { body: { chattype: 'single', from: { userid: 'owner' } } });
      assert.equal(sent.length, 0);
      account.client.emit('message', { body: { chattype: 'single', from: { userid: 'owner' } } });
      account.client.emit('message', { body: { chattype: 'group', chatid: 'group' } });
      account.client.emit('message', { body: { chattype: 'group', chatid: 'group' } });
    } else {
      assert.equal(sent.length, 0);
      account.client.callback({ data: { conversationType: '1', senderStaffId: 'owner' } });
      account.client.callback({ data: { conversationType: '2', conversationId: 'group' } });
      account.client.callback({ data: { conversationType: '2', conversationId: 'group' } });
    }
    await tick();
    assert.deepEqual(sent.map(item => item.target.id), ['owner', 'group']);
    assert.equal(runtime.bindingStatus(provider).privateMessageReceived, true);
    assert.ok(runtime.bindingStatus(provider).pairingReplies.every(reply => reply.outcome === 'accepted'));
  }
});
