import { execFileSync } from 'node:child_process';
import { copyFileSync, readFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'darwin' || !['arm64', 'x64'].includes(process.arch)) throw new Error('请在 Apple Silicon 或 Intel Mac 上构建。');
const root = fileURLToPath(new URL('../', import.meta.url));
const desktop = path.join(root, 'apps/desktop');
const tauri = path.join(desktop, 'src-tauri');
const config = JSON.parse(readFileSync(path.join(tauri, 'tauri.conf.json'), 'utf8'));
const target = process.env.CARGO_TARGET_DIR ? path.resolve(tauri, process.env.CARGO_TARGET_DIR) : path.join(tauri, 'target');
execFileSync('npm', ['run', 'tauri', '--', 'build', '--bundles', 'app', '--config', 'src-tauri/tauri.macos.json', ...process.argv.slice(2)], {
  cwd: desktop, stdio: 'inherit', env: { ...process.env, CARGO_TARGET_DIR: target },
});
const app = path.join(target, 'release/bundle/macos', `${config.productName}.app`);
const triple = process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
const vendorRuntime = path.join(tauri, `binaries/notification-runtime-${triple}`);
execFileSync('/usr/bin/codesign', ['--verify', '--strict', vendorRuntime], { stdio: 'inherit' });
// Tauri 会重新签名 externalBin；恢复原厂签名后仅签外层 App，保留 Bun 的 JIT 权限。
copyFileSync(vendorRuntime, path.join(app, 'Contents/MacOS/notification-runtime'));
execFileSync('/usr/bin/codesign', ['--force', '--sign', process.env.APPLE_SIGNING_IDENTITY || config.bundle.macOS.signingIdentity, app], { stdio: 'inherit' });
execFileSync('/usr/bin/codesign', ['--verify', '--deep', '--strict', app], { stdio: 'inherit' });
// 更新包必须在恢复 Bun 原厂签名、完成外层签名之后生成。
const archive = `${app}.tar.gz`;
execFileSync('/usr/bin/tar', ['-czf', archive, '-C', path.dirname(app), path.basename(app)], { env: { ...process.env, COPYFILE_DISABLE: '1' } });
const version = execFileSync('/usr/libexec/PlistBuddy', ['-c', 'Print CFBundleShortVersionString', path.join(app, 'Contents/Info.plist')], { encoding: 'utf8' }).trim();
const key = process.env.TAURI_SIGNING_PRIVATE_KEY;
const signingEnvironment = { ...process.env, ...(key && existsSync(key) ? { TAURI_SIGNING_PRIVATE_KEY: readFileSync(key, 'utf8') } : {}) };
execFileSync('npm', ['run', 'tauri', '--', 'signer', 'sign', '--app-version', version, archive], { cwd: desktop, stdio: 'inherit', env: signingEnvironment });
console.log(app);
