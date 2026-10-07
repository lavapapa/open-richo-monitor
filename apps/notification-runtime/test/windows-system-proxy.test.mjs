import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { connect } from 'node:net';
import { once } from 'node:events';
import { Runtime } from '../src/main.mjs';
import { WINDOWS_SYSTEM_PROXY } from '../src/windows-system-proxy.mjs';
import { systemProxyAgent } from '../src/windows-system-proxy.mjs';
import WebSocket, { WebSocketServer } from 'ws';
import { defaultHttpInstance as feishuRegistrationHttp } from '@larksuiteoapi/node-sdk';

test('系统代理按完整目标解析，DIRECT 与代理请求各走自己的线路', async () => {
  const target = createServer((req, res) => res.end(JSON.stringify({ path: req.url })));
  const proxy = createServer();
  const tunnels = [];
  proxy.on('connect', (req, downstream, head) => {
    tunnels.push(req.url);
    const [host, port] = req.url.split(':');
    const upstream = connect(Number(port), host, () => {
      downstream.write('HTTP/1.1 200 Connection Established\r\n\r\n');
      upstream.write(head); downstream.pipe(upstream); upstream.pipe(downstream);
    });
    downstream.on('close', () => upstream.destroy());
    upstream.on('error', () => downstream.destroy());
  });
  target.listen(0, '127.0.0.1'); proxy.listen(0, '127.0.0.1');
  await Promise.all([once(target, 'listening'), once(proxy, 'listening')]);
  const urls = [];
  const runtime = new Runtime({ emit() {}, resolveProxy: async (url) => {
    urls.push(url);
    return url.endsWith('/via-proxy') ? `http://127.0.0.1:${proxy.address().port}` : null;
  } });
  try {
    runtime.configure({ network: 'system_proxy', proxyUrl: WINDOWS_SYSTEM_PROXY });
    const base = `http://127.0.0.1:${target.address().port}`;
    assert.deepEqual(await (await runtime.fetch(`${base}/direct`)).json(), { path: '/direct' });
    assert.deepEqual(await (await runtime.fetch(`${base}/via-proxy`)).json(), { path: '/via-proxy' });
    assert.deepEqual(urls, [`${base}/direct`, `${base}/via-proxy`]);
    assert.equal(tunnels.length, 1);
  } finally { await runtime.close(); target.closeAllConnections(); proxy.closeAllConnections(); target.close(); proxy.close(); }
});

test('自动代理解析失败明确失败，不转成直接连接', async () => {
  let requests = 0;
  const target = createServer((req, res) => { requests++; res.end('{}'); });
  target.listen(0, '127.0.0.1'); await once(target, 'listening');
  const runtime = new Runtime({ emit() {}, resolveProxy: async () => { throw new Error('PAC failed'); } });
  try {
    runtime.configure({ network: 'system_proxy', proxyUrl: WINDOWS_SYSTEM_PROXY });
    await assert.rejects(runtime.fetch(`http://127.0.0.1:${target.address().port}/`), /PAC failed/);
    assert.equal(requests, 0);
  } finally { await runtime.close(); target.close(); }
});

test('飞书扫码 SDK 内部 HTTP 实例遵守系统代理并在退出后恢复', async () => {
  let requests = 0;
  const before = feishuRegistrationHttp.defaults.httpAgent;
  const target = createServer((req, res) => { requests++; res.end('{}'); });
  target.listen(0, '127.0.0.1'); await once(target, 'listening');
  const runtime = new Runtime({ emit() {}, resolveProxy: async () => { throw new Error('registration PAC failed'); } });
  try {
    runtime.configure({ network: 'system_proxy', proxyUrl: WINDOWS_SYSTEM_PROXY });
    await assert.rejects(feishuRegistrationHttp.get(`http://127.0.0.1:${target.address().port}/register`), /registration PAC failed/);
    assert.equal(requests, 0);
  } finally { await runtime.close(); target.close(); }
  assert.equal(feishuRegistrationHttp.defaults.httpAgent, before);
});

test('SDK WebSocket 使用同一目标代理解析并保持双向消息', async () => {
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await once(server, 'listening');
  server.on('connection', socket => socket.on('message', message => socket.send(message)));
  const targets = [];
  const socket = new WebSocket(`ws://127.0.0.1:${server.address().port}/events`, {
    agent: systemProxyAgent(async url => { targets.push(url); return null; }),
  });
  try {
    await once(socket, 'open');
    const received = once(socket, 'message');
    socket.send('notification-sdk-probe');
    assert.equal(String((await received)[0]), 'notification-sdk-probe');
    assert.deepEqual(targets, [`http://127.0.0.1:${server.address().port}/events`]);
  } finally {
    socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});
