// 验证自包含通知模块；证书与假账号专用于本地测试，不修改系统信任。
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer as createHttpServer } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import { connect } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';

if (!process.argv[2]) throw new Error('用法：node scripts/test-notification-runtime.mjs <通知运行时> [notification-runtime.mjs]');
const source = path.resolve(process.argv[2]);
const scriptSource = process.argv[3] ? path.resolve(process.argv[3]) : path.join(path.dirname(source), 'notification-runtime.mjs');
const directory = await mkdtemp(path.join(tmpdir(), '理光 通知测试-'));
const executable = path.join(directory, `通知 runtime${process.platform === 'win32' ? '.exe' : ''}`);
const script = path.join(directory, '通知 脚本.mjs');
const certificate = fileURLToPath(new URL('./fixtures/notification-runtime-cert.pem', import.meta.url));
const hostname = 'notification-probe.weixin.qq.com';
const targetIds = ['accepted', 'rejected', 'unknown', 'eof'];
const text = '理光 GR III 库存 3 → 5，中文 & <新品>。\n第二行 📷';
const requests = [];
const connects = [];
const sockets = new Set();
const dingtalkSockets = new Set();
const feishuSockets = new Set();
const wecomSockets = new Set();
const pollResponses = new Set();
const eofResponses = new Set();
const children = [];
let polls = 0;
let dingtalkConnects = 0;
let feishuConnects = 0;
let wecomConnects = 0;
let serverError;

const bounded = (promise, label) => {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label}超过 5 秒`)), 5_000);
  })]).finally(() => clearTimeout(timer));
};
const waitFor = async (predicate, label) => {
  const deadline = Date.now() + 5_000;
  while (!predicate() && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 20));
  if (serverError) throw serverError;
  assert.ok(predicate(), label);
};
const track = (socket) => {
  sockets.add(socket);
  socket.once('close', () => sockets.delete(socket));
};

const api = createHttpsServer({
  cert: await readFile(certificate),
  key: await readFile(new URL('./fixtures/notification-runtime-key.pem', import.meta.url)),
}, async (request, response) => {
  try {
    assert.equal(request.headers.host, hostname);
    assert.equal(request.headers.authorization, 'Bearer fixture-token');
    assert.equal(request.method, 'POST');
    let raw = '';
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    response.setHeader('content-type', 'application/json');
    if (request.url === '/ilink/bot/getupdates') {
      polls += 1;
      pollResponses.add(response);
      const timer = setTimeout(() => response.end(JSON.stringify({ ret: 0, msgs: [], get_updates_buf: '' })), 200);
      response.once('close', () => { clearTimeout(timer); pollResponses.delete(response); });
      return;
    }
    assert.equal(request.url, '/ilink/bot/sendmessage');
    const target = body.msg.to_user_id;
    assert.ok(targetIds.includes(target));
    assert.equal(body.msg.context_token, `fixture-context-${target}`);
    assert.equal(body.msg.item_list[0].text_item.text, text);
    requests.push(target);
    if (target === 'unknown') { response.destroy(); return; }
    if (target === 'eof') {
      eofResponses.add(response);
      response.once('close', () => eofResponses.delete(response));
      return;
    }
    response.end(JSON.stringify({ ret: target === 'rejected' ? 45009 : 0 }));
  } catch (error) { serverError ??= error; response.destroy(); }
});
api.on('connection', track);
api.on('tlsClientError', (error) => { serverError ??= new Error(`本地 TLS 建连失败：${error.message}`); });
const proxy = createHttpServer((_request, response) => {
  serverError ??= new Error('通知模块应通过 CONNECT 请求本地 TLS 服务');
  response.writeHead(400).end();
});
proxy.on('connection', track);
proxy.on('connect', (request, client, head) => {
  try {
    if (['api.dingtalk.com:443', 'open.feishu.cn:443', 'openws.work.weixin.qq.com:443'].includes(request.url)) {
      const activeSockets = request.url === 'api.dingtalk.com:443' ? dingtalkSockets : request.url === 'open.feishu.cn:443' ? feishuSockets : wecomSockets;
      if (request.url === 'api.dingtalk.com:443') dingtalkConnects += 1;
      else if (request.url === 'open.feishu.cn:443') feishuConnects += 1;
      else wecomConnects += 1;
      activeSockets.add(client);
      client.on('error', () => {});
      client.once('end', () => client.end());
      client.once('close', () => activeSockets.delete(client));
      return;
    }
    assert.equal(request.url, `${hostname}:443`, 'CONNECT 必须指向专用测试域名');
    connects.push(request.url);
    const upstream = connect(api.address().port, '127.0.0.1', () => {
      client.write('HTTP/1.1 200 Connection Established\r\n\r\n');
      if (head.length) upstream.write(head);
      client.pipe(upstream).pipe(client);
    });
    track(upstream);
    client.on('error', () => upstream.destroy());
    client.on('close', () => upstream.destroy());
    upstream.on('error', () => client.destroy());
    upstream.on('close', () => client.destroy());
  } catch (error) { serverError ??= error; client.destroy(); }
});

function startRuntime(entryScript = script) {
  const env = { PATH: directory, NODE_EXTRA_CA_CERTS: certificate };
  for (const key of ['SystemRoot', 'WINDIR', 'TEMP', 'TMP']) {
    if (process.env[key]) env[key] = process.env[key];
  }
  const child = spawn(executable, ['--no-env-file', '--no-install', entryScript], { env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  children.push(child);
  const pending = new Map();
  let sequence = 0;
  let failure;
  let stderr = '';
  const fail = (error) => {
    failure = error;
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  };
  child.on('error', fail);
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-4_096); });
  child.stdin.on('error', fail);
  const exited = new Promise((resolve) => child.once('close', (code, signal) => {
    fail(new Error(`通知模块已退出（code=${code}, signal=${signal}）${stderr ? `：${stderr}` : ''}`));
    resolve({ code, signal });
  }));
  readline.createInterface({ input: child.stdout, crlfDelay: Infinity }).on('line', (line) => {
    try {
      const value = JSON.parse(line);
      const request = pending.get(value.id);
      if (!request) return;
      pending.delete(value.id);
      if (value.error) request.reject(new Error(value.error.message));
      else request.resolve(value.result);
    } catch (error) { fail(error); }
  });
  const rpc = async (method, params = {}) => {
    if (failure) throw failure;
    const id = ++sequence;
    const result = new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
    child.stdin.write(`${JSON.stringify({ id, method, params })}\n`);
    try { return await bounded(result, `${method}${method === 'send' ? ` ${params.accountId}/${params.target.id}` : ''} RPC`); }
    finally { pending.delete(id); }
  };
  return { child, rpc, exited };
}

try {
  await copyFile(source, executable);
  await copyFile(scriptSource, script);
  api.listen(0, '127.0.0.1');
  await once(api, 'listening');
  proxy.listen(0, '127.0.0.1');
  await once(proxy, 'listening');
  const account = {
    id: 'fixture-account', provider: 'weixin',
    credentials: {
      token: 'fixture-token', baseUrl: `https://${hostname}/`,
      contextTokens: Object.fromEntries(targetIds.map((id) => [id, `fixture-context-${id}`])),
    },
    targets: targetIds.map((id) => ({ id, kind: 'user', label: `测试 ${id}` })),
  };
  const configuration = (enabled) => ({
    accounts: [{ ...account, enabled }], network: 'system_proxy',
    proxyUrl: `http://127.0.0.1:${proxy.address().port}`,
  });
  const send = (runtime, id) => runtime.rpc('send', {
    accountId: account.id, target: { id, kind: 'user' }, text,
  });
  const runtime = startRuntime();
  assert.deepEqual((await runtime.rpc('status')).accounts, [], '通知模块应独立启动并响应 RPC');
  await runtime.rpc('configure', configuration(true));
  assert.equal((await runtime.rpc('status')).accounts[0].status, 'ready');
  await waitFor(() => polls > 0, '应通过本地代理启动真实 TLS 长轮询');
  assert.equal((await send(runtime, 'accepted')).outcome, 'accepted');
  const rejected = await send(runtime, 'rejected');
  assert.equal(rejected.outcome, 'failed');
  assert.equal(rejected.retryable, false);
  assert.match(rejected.message, /45009/);
  assert.equal((await send(runtime, 'unknown')).outcome, 'unknown');
  const previousPolls = polls;
  await runtime.rpc('configure', configuration(true));
  await waitFor(() => polls > previousPolls, '重新配置后连接应继续运行');
  assert.deepEqual(requests, ['accepted', 'rejected', 'unknown'], '重新配置和网络异常不得重放已发送消息');
  await runtime.rpc('configure', configuration(false));
  assert.equal((await runtime.rpc('status')).accounts[0].status, 'stopped');
  assert.equal((await send(runtime, 'accepted')).outcome, 'failed');
  await waitFor(() => pollResponses.size === 0, '停用应取消长轮询');
  assert.equal(requests.length, 3, '停用账号不得发送 HTTP 消息');
  const shutdownAt = Date.now();
  assert.equal((await runtime.rpc('shutdown')).stopped, true);
  assert.equal((await bounded(runtime.exited, 'shutdown 退出')).code, 0);
  const shutdownMs = Date.now() - shutdownAt;

  const eof = startRuntime();
  await eof.rpc('configure', configuration(true));
  const inFlight = send(eof, 'eof');
  await waitFor(() => requests.length === 4, 'EOF 场景应先发出真实 HTTP 请求');
  const eofAt = Date.now();
  eof.child.stdin.end();
  assert.equal((await inFlight).outcome, 'unknown');
  assert.equal((await bounded(eof.exited, 'EOF 退出')).code, 0);
  await waitFor(() => eofResponses.size === 0 && pollResponses.size === 0, '退出应关闭在途请求与长轮询');
  const eofMs = Date.now() - eofAt;
  assert.deepEqual(requests, ['accepted', 'rejected', 'unknown', 'eof']);
  assert.ok(connects.length > 0);
  const dingtalkAccount = { id: 'fixture-dingtalk', provider: 'dingtalk', credentials: { appId: 'fixture', appSecret: 'fixture' }, targets: [] };
  const dingtalkConfiguration = (enabled) => ({ accounts: [{ ...dingtalkAccount, enabled }], network: 'system_proxy', proxyUrl: configuration(true).proxyUrl });
  const dingtalk = startRuntime();
  await dingtalk.rpc('configure', dingtalkConfiguration(true));
  await waitFor(() => dingtalkSockets.size === 1, '钉钉 SDK 应在本地代理等待 CONNECT 响应');
  const dingtalkDisableAt = Date.now();
  await dingtalk.rpc('configure', dingtalkConfiguration(false));
  assert.equal((await dingtalk.rpc('status')).accounts[0].status, 'stopped');
  await waitFor(() => dingtalkSockets.size === 0, '停用钉钉账号应释放未完成的 CONNECT 连接');
  const dingtalkDisableMs = Date.now() - dingtalkDisableAt;
  await dingtalk.rpc('configure', dingtalkConfiguration(true));
  await waitFor(() => dingtalkSockets.size === 1, '重新启用钉钉账号应开始新的连接');
  const dingtalkShutdownAt = Date.now();
  assert.equal((await dingtalk.rpc('shutdown')).stopped, true);
  assert.equal((await bounded(dingtalk.exited, '钉钉 shutdown 退出')).code, 0);
  await waitFor(() => dingtalkSockets.size === 0, '钉钉 shutdown 应释放未完成的 CONNECT 连接');
  const dingtalkShutdownMs = Date.now() - dingtalkShutdownAt;
  const dingtalkEof = startRuntime();
  await dingtalkEof.rpc('configure', dingtalkConfiguration(true));
  await waitFor(() => dingtalkSockets.size === 1, 'EOF 场景应先进入钉钉 CONNECT 等待');
  const dingtalkEofAt = Date.now();
  dingtalkEof.child.stdin.end();
  assert.equal((await bounded(dingtalkEof.exited, '钉钉 EOF 退出')).code, 0);
  await waitFor(() => dingtalkSockets.size === 0, '钉钉 EOF 应释放未完成的 CONNECT 连接');
  const dingtalkEofMs = Date.now() - dingtalkEofAt;
  assert.equal(dingtalkConnects, 3, '取消连接不得重放或额外重连');
  const feishuProbe = path.join(directory, '飞书取消探测.mjs');
  const feishuFixtures = `class FakeWs { constructor(options) { this.options = options; } start() { this.options.onReady(); } close() {} getConnectionStatus() { return { state: 'connected' }; } }
class FakeDispatcher { register() { return this; } }`;
  await writeFile(feishuProbe, `import { Runtime, run } from ${JSON.stringify(pathToFileURL(script).href)};
${feishuFixtures}
await run(process.stdin, new Runtime({ sdk: { FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } }));
`);
  const feishu = startRuntime(feishuProbe);
  const feishuConfiguration = (enabled) => ({ accounts: [{ id: 'fixture-feishu', provider: 'feishu', credentials: { appId: 'fixture', appSecret: 'fixture' }, targets: [], enabled }], network: 'system_proxy', proxyUrl: configuration(true).proxyUrl });
  await feishu.rpc('configure', feishuConfiguration(true));
  const feishuSend = feishu.rpc('send', { accountId: 'fixture-feishu', target: { id: 'fixture-chat', kind: 'chat' }, text: 'fixture' });
  await waitFor(() => feishuSockets.size > 0, '官方飞书 HTTP SDK 应在本地代理等待 CONNECT 响应');
  const feishuDisableAt = Date.now();
  await feishu.rpc('configure', feishuConfiguration(false));
  assert.equal((await feishuSend).outcome, 'unknown');
  await waitFor(() => feishuSockets.size === 0, '停用飞书账号应释放未完成的 CONNECT 连接');
  const feishuDisableMs = Date.now() - feishuDisableAt;
  assert.equal((await feishu.rpc('shutdown')).stopped, true);
  assert.equal((await bounded(feishu.exited, '飞书 shutdown 退出')).code, 0);
  assert.ok(feishuConnects > 0);
  const feishuGroupProbe = path.join(directory, '飞书群查询取消探测.mjs');
  await writeFile(feishuGroupProbe, `import { Runtime, run } from ${JSON.stringify(pathToFileURL(script).href)};
${feishuFixtures}
const runtime = new Runtime({ sdk: { FeishuWSClient: FakeWs, FeishuEventDispatcher: FakeDispatcher } });
const OfficialClient = runtime.sdk.FeishuClient;
runtime.sdk.FeishuClient = class extends OfficialClient {
  constructor(options) {
    const adapter = options.httpInstance.defaults.adapter;
    options.httpInstance.defaults.adapter = async (config) => {
      const data = config.url.endsWith('/open-apis/im/v1/chats')
        ? await options.httpInstance.request({ ...config, adapter })
        : config.url.includes('tenant_access_token') ? { code: 0, tenant_access_token: 'fixture', expire: 7200 } : { code: 0, bot: { app_name: 'fixture' } };
      return { data, status: 200, statusText: 'OK', headers: {}, config };
    };
    super(options);
  }
};
await run(process.stdin, runtime);
`);
  const feishuGroups = startRuntime(feishuGroupProbe);
  await feishuGroups.rpc('configure', feishuConfiguration(true));
  const pendingGroups = feishuGroups.rpc('detect_groups', { accountId: 'fixture-feishu' }).then(() => false, () => true);
  await waitFor(() => feishuSockets.size > 0, '官方飞书 SDK 群查询应在本地代理等待 CONNECT 响应');
  const feishuGroupDisableAt = Date.now();
  await feishuGroups.rpc('configure', feishuConfiguration(false));
  assert.equal(await pendingGroups, true, '停用应取消群查询 RPC');
  await waitFor(() => feishuSockets.size === 0, '停用应释放群查询 CONNECT 连接');
  const feishuGroupDisableMs = Date.now() - feishuGroupDisableAt;
  assert.equal((await feishuGroups.rpc('shutdown')).stopped, true);
  assert.equal((await bounded(feishuGroups.exited, '飞书群查询 shutdown 退出')).code, 0);
  const initialConnectionDisableMs = {};
  for (const provider of ['feishu', 'wecom']) {
    const transport = startRuntime();
    const credentials = provider === 'feishu' ? { appId: 'cli_0000000000000000', appSecret: 'fixture' } : { botId: 'fixture', secret: 'fixture' };
    const account = { id: `fixture-${provider}-initial`, provider, credentials, targets: [] };
    const configuration = (enabled) => ({ accounts: [{ ...account, enabled }], network: 'system_proxy', proxyUrl: `http://127.0.0.1:${proxy.address().port}` });
    const activeSockets = provider === 'feishu' ? feishuSockets : wecomSockets;
    const previousConnects = provider === 'feishu' ? feishuConnects : wecomConnects;
    await transport.rpc('configure', configuration(true));
    await waitFor(() => provider === 'feishu' ? feishuConnects >= previousConnects + 2 : wecomConnects > previousConnects, `${provider} 官方 SDK 应开始初始连接`);
    assert.ok(activeSockets.size > 0);
    const disabledAt = Date.now();
    await transport.rpc('configure', configuration(false));
    assert.equal((await transport.rpc('status')).accounts[0].status, 'stopped');
    await waitFor(() => activeSockets.size === 0, `${provider} 停用应释放初始连接`);
    initialConnectionDisableMs[provider] = Date.now() - disabledAt;
    assert.equal((await transport.rpc('shutdown')).stopped, true);
    assert.equal((await bounded(transport.exited, `${provider} 初始连接 shutdown 退出`)).code, 0);
  }
  if (serverError) throw serverError;
  process.stdout.write(`${JSON.stringify({ result: 'passed', platform: process.platform, architecture: process.arch, requests: requests.length, connects: connects.length, dingtalkConnects, dingtalkCancelledConnections: dingtalkSockets.size, feishuConnects, feishuCancelledConnections: feishuSockets.size, wecomConnects, wecomCancelledConnections: wecomSockets.size, chinesePath: true, selfContained: true, shutdownMs, eofMs, dingtalkDisableMs, dingtalkShutdownMs, dingtalkEofMs, feishuDisableMs, feishuGroupDisableMs, initialConnectionDisableMs })}\n`);
} finally {
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill();
      await bounded(once(child, 'close'), '测试清理退出');
    }
  }
  for (const socket of sockets) socket.destroy();
  await Promise.all([new Promise((resolve) => proxy.close(resolve)), new Promise((resolve) => api.close(resolve))]);
  await rm(directory, { recursive: true, force: true });
}
