import { copyFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

if (process.platform !== 'darwin') throw new Error('请在 Mac 上运行调试版。');
const root = fileURLToPath(new URL('../', import.meta.url));
const target = path.join(root, 'apps/desktop/src-tauri/target/debug');
const binary = path.join(target, 'ricoh-monitor-desktop');
if (!existsSync(binary)) throw new Error('请先编译桌面 dev 程序。');
const app = path.join(target, 'RichoMonitorDev.app');
mkdirSync(path.join(app, 'Contents/MacOS'), { recursive: true });
mkdirSync(path.join(app, 'Contents/Resources'), { recursive: true });
// UserNotifications 需要真实 App bundle；调试壳仍运行 debug 程序和 Vite 页面。
copyFileSync(binary, path.join(app, 'Contents/MacOS/ricoh-monitor-desktop'));
writeFileSync(path.join(app, 'Contents/Info.plist'), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>ricoh-monitor-desktop</string>
<key>CFBundleIdentifier</key><string>dev.ricohmonitor.debug</string>
<key>CFBundleName</key><string>RichoMonitor Dev</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>`);
// 将调试程序的签名绑定到 App 标识；linker 临时签名无法申请系统通知权限。
execFileSync('/usr/bin/codesign', ['--force', '--sign', '-', '--identifier', 'dev.ricohmonitor.debug', app]);
const args = ['-n', app];
if (process.env.RM_DEV_DATA_DIR) args.push('--env', `RM_DEV_DATA_DIR=${process.env.RM_DEV_DATA_DIR}`);
execFileSync('/usr/bin/open', args);
console.log(app);
