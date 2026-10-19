import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { verifyVersions, verifyRelease } from './verify-release.mjs';

const root = new URL('../', import.meta.url);
const config = JSON.parse(readFileSync(new URL('apps/desktop/src-tauri/tauri.conf.json', root)));
test('版本预检核对全部桌面版本来源', () => {
  assert.equal(verifyVersions(root), config.version);
  const fixture = mkdtempSync(path.join(tmpdir(), 'richo-versions-'));
  try {
    const files = ['apps/desktop/src-tauri/tauri.conf.json', 'apps/desktop/package.json', 'apps/desktop/package-lock.json', 'apps/desktop/src-tauri/Cargo.toml', 'apps/desktop/src-tauri/Cargo.lock'];
    for (const file of files) {
      mkdirSync(path.dirname(path.join(fixture, file)), { recursive: true });
      writeFileSync(path.join(fixture, file), readFileSync(new URL(file, root)));
    }
    const file = path.join(fixture, 'apps/desktop/src-tauri/Cargo.toml');
    writeFileSync(file, readFileSync(file, 'utf8').replace(`version = "${config.version}"`, 'version = "9.9.9"'));
    assert.throws(() => verifyVersions(fixture), /Cargo.toml/);
  } finally { rmSync(fixture, { recursive: true }); }
});

test('正式发行必须有三平台安装包、更新包、签名及一致的清单', () => {
  const directory = mkdtempSync(path.join(tmpdir(), 'richo-assets-'));
  const base = `https://github.com/lavapapa/open-richo-monitor/releases/download/v${config.version}`;
  try {
    const manifest = { version: config.version, platforms: {} };
    const platforms = { 'darwin-aarch64': 'aarch64.app.tar.gz', 'darwin-x86_64': 'x64.app.tar.gz', 'windows-x86_64': 'x64-setup.exe' };
    for (const [platform, suffix] of Object.entries(platforms)) {
      const name = `${config.productName}_${config.version}_${suffix}`;
      writeFileSync(path.join(directory, name), 'fixture');
      writeFileSync(path.join(directory, `${name}.sig`), `${platform}-signature\n`);
      manifest.platforms[platform] = { url: `${base}/${name}`, signature: `${platform}-signature` };
    }
    for (const name of ['INSTALL.txt', `${config.productName}_${config.version}_aarch64.dmg`, `${config.productName}_${config.version}_x64.dmg`]) writeFileSync(path.join(directory, name), 'fixture');
    const save = () => writeFileSync(path.join(directory, 'latest.json'), JSON.stringify(manifest));
    save();
    assert.equal(verifyRelease(directory, config), 10);
    const intel = manifest.platforms['darwin-x86_64'];
    delete manifest.platforms['darwin-x86_64']; save();
    assert.throws(() => verifyRelease(directory, config), /三个平台/);
    manifest.platforms['darwin-x86_64'] = intel;
    intel.url = intel.url.replace(config.version, '9.9.9'); save();
    assert.throws(() => verifyRelease(directory, config), /地址/);
    intel.url = `${base}/${config.productName}_${config.version}_x64.app.tar.gz`;
    intel.signature = 'wrong'; save();
    assert.throws(() => verifyRelease(directory, config), /签名/);
    intel.signature = 'darwin-x86_64-signature'; save();
    writeFileSync(path.join(directory, `${config.productName}_${config.version}_x64.dmg`), '');
    assert.throws(() => verifyRelease(directory, config), /x64.dmg/);
  } finally { rmSync(directory, { recursive: true }); }
});
