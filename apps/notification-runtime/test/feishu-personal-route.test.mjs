import test from 'node:test';
import assert from 'node:assert/strict';
import { Runtime } from '../src/main.mjs';

function fixture(credentials = { appId: 'app', appSecret: 'fixture-secret' }) {
  const sent = [], events = [];
  class Client {
    constructor() { this.im = { v1: { message: { create: async (request) => {
      sent.push(request);
      return request.params.receive_id_type === 'chat_id'
        ? { code: 0, data: { message_id: 'accepted' } }
        : { code: 230101 };
    } } } }; }
  }
  class WS {
    constructor(options) { this.options = options; }
    start() { this.options.onReady(); }
    getConnectionStatus() { return { state: 'connected' }; }
    close() {}
  }
  class Dispatcher { register(handlers) { this.handlers = handlers; return this; } }
  const runtime = new Runtime({ emit: e => events.push(e), sdk: { FeishuClient: Client, FeishuWSClient: WS, FeishuEventDispatcher: Dispatcher } });
  runtime.configure({ accounts: [{ id: 'personal', provider: 'feishu', credentials, targets: [{ id: 'ou_owner', kind: 'user', label: '本人' }], enabled: true }] });
  return { runtime, sent, events };
}

test('飞书个人回调保留单聊 chat_id，发送与重开均使用真实会话', async () => {
  const { runtime, sent, events } = fixture();
  try {
    const account = runtime.accounts.get('personal');
    account.dispatcher.handlers['im.message.receive_v1']({ sender: { sender_id: { open_id: 'ou_owner' } }, message: { chat_type: 'p2p', chat_id: 'oc_private' } });
    const result = await runtime.send({ accountId: 'personal', target: { id: 'ou_owner', kind: 'user' }, text: '库存测试' });
    assert.equal(result.outcome, 'accepted');
    assert.equal(sent[0].params.receive_id_type, 'chat_id');
    assert.equal(sent[0].data.receive_id, 'oc_private');
    const update = events.find(e => e.event === 'credentials' && e.data.credentials.p2pChatIds);
    assert.deepEqual(update.data.credentials.p2pChatIds, { ou_owner: 'oc_private' });
    assert.equal(account.targets[0].kind, 'user');
    const reopened = fixture({ appId: 'app', appSecret: 'fixture-secret', ...update.data.credentials });
    try {
      assert.equal((await reopened.runtime.send({ accountId: 'personal', target: { id: 'ou_owner', kind: 'user' }, text: '重开测试' })).outcome, 'accepted');
      assert.equal(reopened.sent[0].data.receive_id, 'oc_private');
    } finally { await reopened.runtime.close(); }
  } finally { await runtime.close(); }
});

test('飞书拒绝首次发送保留错误码与平台检查项，不推定私信原因', async () => {
  const { runtime, sent } = fixture();
  try {
    const result = await runtime.send({ accountId: 'personal', target: { id: 'ou_owner', kind: 'user' }, text: '库存测试' });
    assert.equal(result.outcome, 'failed');
    assert.match(result.message, /230101/);
    assert.match(result.message, /可用范围/);
    assert.doesNotMatch(result.message, /私信/);
    assert.equal(sent.length, 1);
  } finally { await runtime.close(); }
});

test('打开飞书机器人会话即可取得投递地址，无需先发送私信', async () => {
  const { runtime, sent, events } = fixture();
  try {
    const account = runtime.accounts.get('personal');
    account.dispatcher.handlers['im.chat.access_event.bot_p2p_chat_entered_v1']({ operator_id: { open_id: 'ou_owner' }, chat_id: 'oc_entered' });
    assert.equal((await runtime.send({ accountId: 'personal', target: { id: 'ou_owner', kind: 'user' }, text: '库存测试' })).outcome, 'accepted');
    assert.equal(sent[0].data.receive_id, 'oc_entered');
    assert.ok(events.some(e => e.event === 'credentials' && e.data.credentials.p2pChatIds?.ou_owner === 'oc_entered'));
  } finally { await runtime.close(); }
});
