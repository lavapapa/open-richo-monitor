import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const bun = process.env.RM_BUN_PATH || 'bun';
const version = execFileSync(bun, ['--version'], { encoding: 'utf8' }).trim();
const requiredVersion = '1.4.2';
if (version !== requiredVersion) {
  throw new Error(`通知模块需使用 Bun ${requiredVersion} 构建，当前为 ${version}。可通过 RM_BUN_PATH 指定构建工具。`);
}
const target = {
  'darwin-arm64': ['aarch64-apple-darwin', ''],
  'darwin-x64': ['x86_64-apple-darwin', ''],
  'win32-x64': ['x86_64-pc-windows-msvc', '.exe'],
}[`${process.platform}-${process.arch}`];
if (!target) throw new Error(`通知模块尚未配置 ${process.platform}/${process.arch} 构建目标。`);
const directory = path.join(root, 'apps/desktop/src-tauri/binaries');
mkdirSync(directory, { recursive: true });
const runtime = path.join(root, 'apps/notification-runtime');
const noticeFiles = [
  path.join(root, 'LICENSE'),
  path.join(root, 'third_party/bun/LICENSE.md'),
  path.join(root, 'third_party/bun/SOURCE.txt'),
  path.join(root, 'apps/desktop/src-tauri/icons/TRAY-LICENSE.txt'),
  path.join(root, 'apps/desktop/THIRD_PARTY_ASSETS.txt'),
  ...['NOTICE.md', 'THIRD_PARTY_NOTICES.md', 'LICENSE.dsh-im', 'LICENSE.larksuite', 'LICENSE.dingtalk'].map(name => path.join(runtime, name)),
];
function collectLicenses(folder) {
  for (const entry of readdirSync(folder, { withFileTypes: true })) {
    const file = path.join(folder, entry.name);
    if (entry.isDirectory()) collectLicenses(file);
    else if (entry.isFile() && /^(license|copying)([.-].*)?$/i.test(entry.name)) noticeFiles.push(file);
  }
}
collectLicenses(path.join(runtime, 'node_modules'));
collectLicenses(path.join(root, 'apps/desktop/node_modules'));
writeFileSync(path.join(directory, 'notification-runtime-NOTICES.txt'), noticeFiles.map(file =>
  `\n${path.relative(root, file)}\n\n${readFileSync(file, 'utf8')}\n`
).join('\n'));
execFileSync(bun, [
  'run', path.join(root, 'apps/notification-runtime/build.mjs'),
  path.join(directory, 'notification-runtime.mjs'),
], { cwd: root, stdio: 'inherit' });
// Bun 自报启动文件路径，避免依赖调用方的 PATH 或工作目录。
const bunExecutable = execFileSync(bun, ['--no-env-file', '-e', 'process.stdout.write(process.execPath)'], { encoding: 'utf8' });
copyFileSync(bunExecutable, path.join(directory, `notification-runtime-${target[0]}${target[1]}`));
