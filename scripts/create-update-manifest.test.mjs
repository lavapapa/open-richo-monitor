import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

test('更新清单要求 Apple Silicon 与 Windows 构件和签名齐全，使用实际发布文件名', () => {
  const script = fileURLToPath(new URL('./create-update-manifest.mjs', import.meta.url));
  const config = JSON.parse(readFileSync(new URL('../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
  const directory = mkdtempSync(path.join(tmpdir(), 'ricoh-release-test-'));
  try {
    const missing = spawnSync(process.execPath, [script, directory], { encoding: 'utf8' });
    assert.notEqual(missing.status, 0);
    assert.match(missing.stderr, /缺少更新构件/);
    for (const suffix of ['aarch64.app.tar.gz', 'x64-setup.exe']) {
      const file = path.join(directory, `${config.productName}_${config.version}_${suffix}`);
      writeFileSync(file, '测试构件');
      writeFileSync(`${file}.sig`, 'fixture-signature\n');
    }
    execFileSync(process.execPath, [script, directory]);
    const manifest = JSON.parse(readFileSync(path.join(directory, 'latest.json'), 'utf8'));
    assert.equal(manifest.version, config.version);
    assert.deepEqual(Object.keys(manifest.platforms), ['darwin-aarch64', 'windows-x86_64']);
    for (const entry of Object.values(manifest.platforms)) {
      assert.equal(entry.signature, 'fixture-signature');
      assert.ok(entry.url.startsWith(`https://github.com/lavapapa/open-richo-monitor/releases/download/v${config.version}/`));
    }
    const mirrorBase = `https://gitee.com/marvinfore/open-richo-monitor/releases/download/v${config.version}`;
    execFileSync(process.execPath, [script, directory, '', mirrorBase]);
    const mirror = JSON.parse(readFileSync(path.join(directory, 'latest.json'), 'utf8'));
    for (const entry of Object.values(mirror.platforms)) {
      assert.ok(entry.url.startsWith(`${mirrorBase}/`));
      assert.equal(entry.signature, 'fixture-signature');
    }
    const intel = path.join(directory, `${config.productName}_${config.version}_x64.app.tar.gz`);
    writeFileSync(intel, 'Intel 测试构件');
    const missingIntelSignature = spawnSync(process.execPath, [script, directory], { encoding: 'utf8' });
    assert.notEqual(missingIntelSignature.status, 0);
    writeFileSync(`${intel}.sig`, 'intel-signature\n');
    execFileSync(process.execPath, [script, directory]);
    const withIntel = JSON.parse(readFileSync(path.join(directory, 'latest.json'), 'utf8'));
    assert.equal(withIntel.platforms['darwin-x86_64'].signature, 'intel-signature');
    assert.ok(withIntel.platforms['darwin-x86_64'].url.endsWith(`/${path.basename(intel)}`));
    assert.deepEqual(Object.keys(withIntel.platforms).sort(), ['darwin-aarch64', 'darwin-x86_64', 'windows-x86_64']);
    const signature = path.join(directory, `${config.productName}_${config.version}_x64-setup.exe.sig`);
    writeFileSync(signature, '');
    const unsigned = spawnSync(process.execPath, [script, directory], { encoding: 'utf8' });
    assert.notEqual(unsigned.status, 0);
    assert.match(unsigned.stderr, /缺少更新签名/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
