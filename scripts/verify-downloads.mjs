import assert from 'node:assert/strict';
import { appendFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { verifyManifest } from './verify-release.mjs';

export async function verifySource(source, fetcher = fetch) {
  async function request(url, method) {
    let response;
    try {
      response = await fetcher(url, { method, signal: AbortSignal.timeout(30000) });
    } catch (error) {
      throw new Error(`${method} ${url}：${error.message}`, { cause: error });
    }
    assert.ok(response.ok, `${method} HTTP ${response.status}：${url}`);
    return response;
  }
  const manifestPath = new URL(source).hostname === 'github.com'
    ? 'releases/latest/download/latest.json'
    : 'releases/download/latest/latest.json';
  const manifest = await (await request(`${source}/${manifestPath}`, 'GET')).json();
  assert.match(manifest.version, /^\d+\.\d+\.\d+$/, '公开清单缺少正式版本号');
  const config = { productName: 'RichoMonitor', version: manifest.version };
  const base = `${source}/releases/download/v${manifest.version}`;
  const files = Object.values(verifyManifest(manifest, config, base));
  files.push(`RichoMonitor_${manifest.version}_aarch64.dmg`, `RichoMonitor_${manifest.version}_x64.dmg`);
  await Promise.all(files.map(async file => {
    const url = `${base}/${file}`;
    const response = await request(url, 'HEAD');
    assert.ok(!response.headers.get('content-type')?.includes('text/html'), `下载地址返回 HTML 页面：${url}`);
  }));
  return { source, version: manifest.version, files: files.length };
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const sources = ['https://github.com/lavapapa/open-richo-monitor'];
  if (!process.argv.includes('--github-only')) sources.push('https://gitee.com/marvinfore/open-richo-monitor');
  const results = await Promise.allSettled(sources.map(source => verifySource(source)));
  const lines = results.map((result, index) => {
    if (result.status === 'fulfilled') return `通过：${result.value.source} · ${result.value.version} · ${result.value.files} 个下载地址`;
    process.exitCode = 1;
    return `失败：${sources[index]} · ${result.reason.message}`;
  });
  if (results.every(result => result.status === 'fulfilled') && results.length === 2) {
    if (results[0].value.version !== results[1].value.version) { lines.push('失败：GitHub 与 Gitee 最新版本未同步'); process.exitCode = 1; }
  }
  console.log(lines.join('\n'));
  if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, lines.map(line => `- ${line}\n`).join(''));
}
