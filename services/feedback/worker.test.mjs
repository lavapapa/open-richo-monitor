import { test } from 'node:test';
import assert from 'node:assert/strict';
import worker from './worker.mjs';

test('反馈成功持久化后才返回确认，公网没有读取入口', async () => {
  const values = new Map();
  const env = { FEEDBACK: { put: async (key, value) => values.set(key, value) } };
  const response = await worker.fetch(new Request('https://example.test/feedback', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ message: '二维码太小', version: '0.1.3', platform: 'macos' }) }), env);
  assert.equal(response.status, 200);
  assert.equal((await response.json()).ok, true);
  assert.equal(JSON.parse([...values.values()][0]).message, '二维码太小');
  assert.equal((await worker.fetch(new Request('https://example.test/feedback'), env)).status, 405);
});

test('无效内容拒收，存储失败不会虚报收到', async () => {
  const request = (message) => new Request('https://example.test/feedback', { method: 'POST', body: JSON.stringify({ message }) });
  assert.equal((await worker.fetch(request('  '), {})).status, 400);
  assert.equal((await worker.fetch(request('测'.repeat(4001)), {})).status, 400);
  assert.equal((await worker.fetch(request('正常意见'), { FEEDBACK: { put: async () => { throw new Error('额度耗尽'); } } })).status, 503);
});
