import { execFileSync } from 'node:child_process';
import { cpSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'darwin' || !['arm64', 'x64'].includes(process.arch)) throw new Error('请在 Mac 上制作 DMG。');
const root = fileURLToPath(new URL('../', import.meta.url));
const tauri = path.join(root, 'apps/desktop/src-tauri');
const config = JSON.parse(readFileSync(path.join(tauri, 'tauri.conf.json'), 'utf8'));
const target = process.env.CARGO_TARGET_DIR ? path.resolve(tauri, process.env.CARGO_TARGET_DIR) : path.join(tauri, 'target');
const app = path.join(target, 'release/bundle/macos', `${config.productName}.app`);
execFileSync('/usr/bin/codesign', ['--verify', '--deep', '--strict', app], { stdio: 'inherit' });
const binaryArch = execFileSync('/usr/bin/lipo', ['-archs', path.join(app, 'Contents/MacOS/ricoh-monitor-desktop')], { encoding: 'utf8' }).trim();
const arch = { arm64: 'aarch64', x86_64: 'x64' }[binaryArch];
if (!arch) throw new Error(`DMG 需要单一架构 App，当前为 ${binaryArch}。`);
const destination = path.join(root, `dist/macos-${arch}`);
mkdirSync(destination, { recursive: true });
const dmg = path.join(destination, `${config.productName}_${config.version}_${arch}.dmg`);
const staging = mkdtempSync(path.join(tmpdir(), 'ricoh-trial-dmg-'));
try {
  cpSync(app, path.join(staging, `${config.productName}.app`), { recursive: true });
  symlinkSync('/Applications', path.join(staging, 'Applications'));
  cpSync(path.join(root, 'docs/private-trial.txt'), path.join(staging, '安装说明.txt'));
  cpSync(path.join(root, 'LICENSE'), path.join(staging, 'LICENSE.txt'));
  // 为完整 App 预留容量，避免 srcfolder 自动估算出的镜像空间不足。
  execFileSync('/usr/bin/hdiutil', ['create', '-ov', '-volname', config.productName, '-srcfolder', staging, '-size', '256m', '-format', 'UDZO', dmg], { stdio: 'inherit' });
  const update = `${config.productName}_${config.version}_${arch}.app.tar.gz`;
  cpSync(`${app}.tar.gz`, path.join(destination, update));
  cpSync(`${app}.tar.gz.sig`, path.join(destination, `${update}.sig`));
} finally {
  rmSync(staging, { recursive: true, force: true });
}
console.log(dmg);
