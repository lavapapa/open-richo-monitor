import assert from 'node:assert/strict';
import test from 'node:test';
import { verifySource } from './verify-downloads.mjs';

const source = 'https://github.com/lavapapa/open-richo-monitor';
const version = '1.2.3';
function manifest() {
  return { version, platforms: Object.fromEntries(Object.entries({ 'darwin-aarch64': 'aarch64.app.tar.gz', 'darwin-x86_64': 'x64.app.tar.gz', 'windows-x86_64': 'x64-setup.exe' }).map(([key, suffix]) => [key, { url: `${source}/releases/download/v${version}/RichoMonitor_${version}_${suffix}`, signature: 'fixture' }])) };
}
test('公开检查逐一检查三平台安装与更新地址，拒绝缺失或 HTML 登录页', async () => {
  const requests = [];
  let content = manifest();
  let html = false;
  const fetcher = async (url, options) => {
    requests.push({ url, method: options.method });
    if (options.method === 'GET') return new Response(JSON.stringify(content));
    return new Response(null, { headers: { 'content-type': html ? 'text/html' : 'application/octet-stream' } });
  };
  assert.deepEqual(await verifySource(source, fetcher), { source, version, files: 5 });
  assert.equal(requests.filter(r => r.method === 'HEAD').length, 5);
  delete content.platforms['darwin-x86_64'];
  await assert.rejects(verifySource(source, fetcher), /三个平台/);
  content = manifest(); html = true;
  await assert.rejects(verifySource(source, fetcher), /HTML/);
});
test('公开检查保留网络失败，错误中包含出问题的下载地址', async () => {
  await assert.rejects(verifySource(source, async () => new Response('not found', { status: 404 })), /404.*latest.json/);
});
