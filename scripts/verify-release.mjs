import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
export function releasePlatforms({ productName, version }) {
  return Object.fromEntries(Object.entries({ 'darwin-aarch64': 'aarch64.app.tar.gz', 'darwin-x86_64': 'x64.app.tar.gz', 'windows-x86_64': 'x64-setup.exe' }).map(([platform, suffix]) => [platform, `${productName}_${version}_${suffix}`]));
}
export function verifyManifest(manifest, config, base) {
  assert.equal(manifest.version, config.version, '更新清单版本与应用版本不同');
  const platforms = releasePlatforms(config);
  assert.deepEqual(Object.keys(manifest.platforms ?? {}).sort(), Object.keys(platforms).sort(), '更新清单必须包含三个平台');
  for (const [platform, filename] of Object.entries(platforms)) {
    const entry = manifest.platforms[platform];
    assert.equal(entry.url, `${base}/${filename}`, `${platform} 更新地址与发行版本或文件名不同`);
    assert.ok(typeof entry.signature === 'string' && entry.signature.trim(), `${platform} 缺少更新签名`);
  }
  return platforms;
}
export function verifyVersions(directory = root) {
  const location = directory instanceof URL ? fileURLToPath(directory) : directory;
  const read = file => readFileSync(path.join(location, file), 'utf8');
  const version = JSON.parse(read('apps/desktop/src-tauri/tauri.conf.json')).version;
  const lock = JSON.parse(read('apps/desktop/package-lock.json'));
  const toml = read('apps/desktop/src-tauri/Cargo.toml').match(/\[package\]([\s\S]*?)(?=\n\[|$)/)?.[1];
  const cargoLock = read('apps/desktop/src-tauri/Cargo.lock').split('[[package]]').find(block => /^name = "ricoh-monitor-desktop"$/m.test(block));
  for (const [name, actual] of Object.entries({
    'package.json': JSON.parse(read('apps/desktop/package.json')).version,
    'package-lock.json': lock.version,
    'package-lock.json packages': lock.packages[''].version,
    'Cargo.toml': toml?.match(/^version\s*=\s*"([^"]+)"/m)?.[1],
    'Cargo.lock': cargoLock?.match(/^version\s*=\s*"([^"]+)"/m)?.[1],
  })) assert.equal(actual, version, `${name} 版本未同步，请与 tauri.conf.json 保持一致`);
  return version;
}
export function verifyRelease(directory, config, base = `https://github.com/lavapapa/open-richo-monitor/releases/download/v${config.version}`) {
  const manifest = JSON.parse(readFileSync(path.join(directory, 'latest.json')));
  const platforms = verifyManifest(manifest, config, base);
  const files = ['latest.json', 'INSTALL.txt', ...Object.values(platforms).flatMap(file => [file, `${file}.sig`]), ...['aarch64', 'x64'].map(arch => `${config.productName}_${config.version}_${arch}.dmg`)];
  for (const file of files) assert.ok(statSync(path.join(directory, file)).size > 0, `发行文件为空：${file}`);
  for (const [platform, file] of Object.entries(platforms)) assert.equal(manifest.platforms[platform].signature, readFileSync(path.join(directory, `${file}.sig`), 'utf8').trim(), `${platform} 清单签名与构件签名文件不同`);
  return files.length;
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const version = verifyVersions();
    if (process.argv[2]) {
      const config = JSON.parse(readFileSync(path.join(root, 'apps/desktop/src-tauri/tauri.conf.json')));
      console.log(`发行构件检查通过：${verifyRelease(process.argv[2], config, process.argv[3])} 个文件，版本 ${version}`);
    } else console.log(`桌面版本一致：${version}`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
