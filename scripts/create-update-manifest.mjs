import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const config = JSON.parse(readFileSync(path.join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'));
const directory = path.resolve(process.argv[2] || path.join(root, 'dist/release'));
const notes = process.argv[3] ? readFileSync(process.argv[3], 'utf8') : '更新应用及内置通知模块。';
const base = process.argv[4] || `https://github.com/lavapapa/open-richo-monitor/releases/download/v${config.version}`;
const files = {
  'darwin-aarch64': `${config.productName}_${config.version}_aarch64.app.tar.gz`,
  'windows-x86_64': `${config.productName}_${config.version}_x64-setup.exe`,
};
const platforms = Object.fromEntries(Object.entries(files).map(([target, filename]) => {
  if (!existsSync(path.join(directory, filename))) throw new Error(`缺少更新构件：${filename}`);
  const signature = readFileSync(path.join(directory, `${filename}.sig`), 'utf8').trim();
  if (!signature) throw new Error(`缺少更新签名：${filename}`);
  return [target, { url: `${base}/${filename}`, signature }];
}));
writeFileSync(path.join(directory, 'latest.json'), JSON.stringify({ version: config.version, notes, pub_date: new Date().toISOString(), platforms }, null, 2) + '\n');
console.log(path.join(directory, 'latest.json'));
