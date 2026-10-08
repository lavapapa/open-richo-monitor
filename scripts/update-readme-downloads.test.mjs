import { test } from 'node:test';
import assert from 'node:assert/strict';
import { downloads, updateReadme } from './update-readme-downloads.mjs';
test('Windows 检出后的 CRLF 不影响下载表格校验', () => {
  const current = `说明\r\n${downloads('0.1.4').replaceAll('\n', '\r\n')}\r\n`;
  assert.equal(updateReadme(current, '0.1.4'), current);
  assert.match(updateReadme(current, '0.1.5'), /RichoMonitor_0\.1\.5/);
});
test('三种普通安装包以清楚的系统名称呈现，版本共同更新', () => {
  const table = downloads('0.2.0');
  for (const suffix of ['aarch64.dmg', 'x64.dmg', 'x64-setup.exe']) assert.match(table, new RegExp(`RichoMonitor_0\\.2\\.0_${suffix.replaceAll('.', '\\.')}`));
  assert(!table.includes('.sig'));
  assert(!table.includes('.tar.gz'));
});
