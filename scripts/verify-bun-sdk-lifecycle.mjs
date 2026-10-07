import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { Runtime } from '../apps/notification-runtime/src/main.mjs';

const require = createRequire(new URL('../apps/notification-runtime/package.json', import.meta.url));
const { WebSocketServer } = require('ws');
const { WSClient } = require('@wecom/aibot-node-sdk');
const waitFor = async (predicate, label) => {
  const end = Date.now() + 5_000;
  while (!predicate() && Date.now() < end) await new Promise((resolve) => setTimeout(resolve, 20));
  assert.ok(predicate(), label);
};

const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
await new Promise((resolve) => server.once('listening', resolve));
let connections = 0;
let heartbeats = 0;
let deliveries = 0;
let socket;
server.on('connection', (client) => {
  connections += 1;
  socket = client;
  client.on('message', (raw) => {
    const frame = JSON.parse(String(raw));
    if (frame.cmd === 'aibot_subscribe') {
      client.send(JSON.stringify({ headers: frame.headers, errcode: 0, errmsg: 'ok' }));
    } else if (frame.cmd === 'ping') {
      heartbeats += 1;
      client.send(JSON.stringify({ headers: frame.headers, errcode: 0, errmsg: 'ok' }));
    } else if (frame.cmd === 'aibot_send_msg') {
      assert.equal(frame.body.msgtype, 'markdown', '企微主动推送需使用官方支持的 Markdown 消息');
      assert.equal(typeof frame.body.markdown?.content, 'string');
      deliveries += 1;
      client.send(JSON.stringify({ headers: frame.headers, errcode: 0, errmsg: 'ok' }));
    }
  });
});

class LocalWecom extends WSClient {
  constructor(options) {
    super({ ...options, wsUrl: `ws://127.0.0.1:${server.address().port}`, heartbeatInterval: 60, reconnectInterval: 30 });
  }
}
const events = [];
const cycles = Number(process.argv[2] ?? 1);
const intervalMs = Number(process.argv[3] ?? 0);
assert.ok(Number.isInteger(cycles) && cycles > 0 && cycles <= 1_000);
assert.ok(Number.isFinite(intervalMs) && intervalMs >= 0);
const initialMemory = process.memoryUsage().rss;
const runtime = new Runtime({ sdk: { WecomWSClient: LocalWecom }, emit: (event) => events.push(event) });
const configuration = { accounts: [{ id: 'fixture', provider: 'wecom', credentials: { botId: 'fixture-bot', secret: 'fixture-secret' }, targets: [{ id: 'fixture-target', kind: 'chat', label: '测试群' }], enabled: true }] };
try {
  await runtime.call('configure', configuration);
  await waitFor(() => runtime.status().accounts[0]?.status === 'ready', '实际 SDK 应认证就绪');
  const send = () => runtime.call('send', { accountId: 'fixture', target: { id: 'fixture-target', kind: 'chat' }, text: '测试库存 3 → 5' });
  assert.equal((await send()).outcome, 'accepted');
  await waitFor(() => heartbeats >= 2, '实际 SDK 应维持心跳');
  for (let cycle = 0; cycle < cycles; cycle += 1) {
    if (intervalMs) await new Promise((resolve) => setTimeout(resolve, intervalMs));
    socket.terminate();
    await waitFor(() => connections >= cycle + 2 && runtime.status().accounts[0]?.status === 'ready', '实际 SDK 应断线恢复');
    assert.equal((await send()).outcome, 'accepted');
  }
  await runtime.call('configure', configuration);
  assert.equal(connections, cycles + 1, '相同配置应复用已连接账号');
  assert.equal(deliveries, cycles + 1, '重连及重新配置不得重放已接受消息');
  assert.equal(JSON.stringify(events).includes('fixture-secret'), false);
  await runtime.close();
  await waitFor(() => server.clients.size === 0, '退出应断开实际 SDK 套接字');
  process.stdout.write(JSON.stringify({ runtime: process.versions.bun ?? process.version, connections, heartbeats, deliveries, initialRss: initialMemory, finalRss: process.memoryUsage().rss, result: 'passed' }) + '\n');
} finally {
  await runtime.close();
  for (const client of server.clients) client.terminate();
  await new Promise((resolve) => server.close(resolve));
}
