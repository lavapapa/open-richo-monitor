import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../apps/notification-runtime/package.json', import.meta.url));
const { Client } = require('@larksuiteoapi/node-sdk');

const database = process.argv[2];
const name = process.argv[3];
if (!database || !name) throw new Error('需提供 App 数据库路径和已授权测试群名。');
const packed = execFileSync('sqlite3', [database,
  "SELECT c.value FROM credentials c JOIN notification_channels n ON n.credential_ref=c.reference WHERE c.kind='channel' AND n.provider='feishu' LIMIT 1",
], { encoding: 'utf8' }).trim();
const values = JSON.parse(packed);
const credentials = values._sdkCredentials ? JSON.parse(values._sdkCredentials) : values;
const client = new Client({ appId: credentials.appId, appSecret: credentials.appSecret, logger: { info() {}, debug() {}, warn() {}, error() {} } });
const groups = [];
let pageToken;
try {
  do {
    const response = await client.im.v1.chat.list({ params: { page_size: 100, ...(pageToken ? { page_token: pageToken } : {}) } });
    if (response.code !== 0) {
      process.stdout.write(JSON.stringify({ operation: 'discover', outcome: 'failed', code: response.code }) + '\n');
      process.exitCode = 1;
      break;
    }
    groups.push(...(response.data?.items ?? []));
    pageToken = response.data?.has_more ? response.data.page_token : null;
  } while (pageToken);
  if (!process.exitCode) {
    const matches = groups.filter((group) => group.name === name);
    process.stdout.write(JSON.stringify({ operation: 'discover', outcome: 'accepted', count: groups.length, matchingTestGroups: matches.length, group: name }) + '\n');
    if (process.argv.includes('--send')) {
      if (matches.length !== 1) throw new Error('测试群未找到或重名，停止发送。');
      const response = await client.im.v1.message.create({ params: { receive_id_type: 'chat_id' }, data: {
        receive_id: matches[0].chat_id, msg_type: 'text',
        content: JSON.stringify({ text: '理光库存监控群聊测试：模拟库存从 3 增至 5。此为测试消息，不代表商品实际上架。' }),
      } });
      process.stdout.write(JSON.stringify({ operation: 'send', outcome: response.code === 0 ? 'accepted' : 'failed', code: response.code, group: name }) + '\n');
    }
  }
} catch (error) {
  process.stdout.write(JSON.stringify({ operation: 'discover_or_send', outcome: 'failed', code: error?.response?.data?.code ?? null }) + '\n');
  process.exitCode = 1;
}
