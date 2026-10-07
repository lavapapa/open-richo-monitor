// 已构建的隔离测试实例，通过 tauri-driver 验证真实 WebView 与原生命令。
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

if (process.platform !== 'win32') throw new Error('此验收需要 Windows 桌面会话');
const application = resolve(process.argv[2]);
const evidence = resolve(process.argv[3] ?? 'dist/windows-verification');
const base = process.env.TAURI_WEBDRIVER_URL ?? 'http://127.0.0.1:4444';
let session;
const result = { startedAt: new Date().toISOString(), steps: [] };
await mkdir(evidence, { recursive: true });
async function request(method, path, body) {
  const response = await fetch(`${base}${path}`, { method, headers: { 'content-type': 'application/json' },
    ...(body ? { body: JSON.stringify(body) } : {}), signal: AbortSignal.timeout(path === '/session' ? 120_000 : 45_000) });
  const value = await response.json();
  if (!response.ok || value.value?.error) throw new Error(`${path}: ${value.value?.message ?? response.status}`);
  return value.value;
}
async function invoke(command, args = {}) {
  const value = await request('POST', `/session/${session}/execute/async`, {
    script: `const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(result=>done({ok:true,result}),error=>done({ok:false,error:String(error)}));`,
    args: [command, args],
  });
  if (!value.ok) throw new Error(`${command}: ${value.error}`);
  return value.result;
}
async function step(name, action) {
  result.phase = name;
  const started = performance.now();
  await action();
  result.steps.push({ name, milliseconds: Math.round(performance.now() - started), passed: true });
}
try {
  for (let attempt = 0; attempt < 60; attempt++) {
    try { await request('GET', '/status'); break; } catch (error) { if (attempt === 59) throw error; await sleep(500); }
  }
  result.phase = '创建 WebDriver 会话';
  const created = await request('POST', '/session', { capabilities: { alwaysMatch: {
    browserName: 'wry', 'tauri:options': { application },
  } } });
  session = created.sessionId;
  result.phase = '设置 WebDriver 超时';
  await request('POST', `/session/${session}/timeouts`, { script: 30_000, implicit: 5_000 });
  await step('真实 WebView 可读取核心状态', async () => {
    const state = await invoke('get_desktop_snapshot');
    assert.ok(state.catalog.length > 0);
  });
  await step('隔离实例完成选品、引导及暂停', async () => {
    await invoke('set_auto_start_monitoring', { enabled: false });
    await invoke('set_onboarding_products', { productIds: ['66'] });
    await invoke('complete_setup');
    await invoke('monitoring_action', { action: 'pause' });
    const state = await invoke('get_desktop_snapshot');
    assert.equal(state.setupCompleted, true);
    assert.equal(state.runtime.state, 'paused');
  });
  await step('突出提醒经过页面确认并能关闭', async () => {
    await invoke('test_prominent_alert', { productId: '66' });
    const alert = await invoke('get_prominent_alert');
    assert.equal(alert.productId, '66');
    assert.ok(alert.eventId < 0);
    await invoke('dismiss_prominent_alert', { eventId: alert.eventId });
    assert.equal(await invoke('get_prominent_alert'), null);
    // 记录原生驱动可见的 WebView 数量，实际屏幕覆盖另由桌面验收记录。
    const handles = await request('GET', `/session/${session}/window/handles`);
    result.windowHandles = handles.length;
    const state = await request('POST', `/session/${session}/execute/sync`, {
      script: 'return document.body.innerText;', args: [],
    });
    assert.ok(state.length > 0);
  });
  const screenshot = await request('GET', `/session/${session}/screenshot`);
  await writeFile(resolve(evidence, 'desktop.png'), Buffer.from(screenshot, 'base64'));
  result.completedAt = new Date().toISOString();
} catch (error) {
  result.error = error.message;
  process.exitCode = 1;
} finally {
  if (session) await request('DELETE', `/session/${session}`).catch(() => {});
  await writeFile(resolve(evidence, 'result.json'), JSON.stringify(result, null, 2));
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
