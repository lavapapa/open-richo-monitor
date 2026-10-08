import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export function downloads(version) {
  const base = `https://github.com/lavapapa/open-richo-monitor/releases/download/v${version}/RichoMonitor_${version}`;
  const mirror = `https://gitee.com/marvinfore/open-richo-monitor/releases/download/v${version}/RichoMonitor_${version}`;
  return `<!-- downloads:start -->
| 你的电脑 | 点击下载 | 国内镜像 |
| --- | --- | --- |
| Mac · Apple Silicon（M1、M2、M3、M4 等） | [下载 Mac Apple Silicon 版](${base}_aarch64.dmg) | [Gitee 下载](${mirror}_aarch64.dmg) |
| Mac · Intel | [下载 Mac Intel 版](${base}_x64.dmg) | [Gitee 下载](${mirror}_x64.dmg) |
| Windows · 64 位 | [下载 Windows 版](${base}_x64-setup.exe) | [Gitee 下载](${mirror}_x64-setup.exe) |
<!-- downloads:end -->`;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const readme = new URL('../README.md', import.meta.url);
  const version = JSON.parse(readFileSync(new URL('../apps/desktop/src-tauri/tauri.conf.json', import.meta.url))).version;
  const current = readFileSync(readme, 'utf8');
  const output = current.replace(/<!-- downloads:start -->[\s\S]*?<!-- downloads:end -->/, downloads(version));
  if (!current.includes('<!-- downloads:start -->')) throw new Error('README 缺少下载表格标记。');
  if (process.argv.includes('--check')) {
    if (output !== current) throw new Error('下载链接版本未同步，请运行 node scripts/update-readme-downloads.mjs。');
  } else writeFileSync(readme, output);
}
