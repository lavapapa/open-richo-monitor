import { basename, dirname, resolve } from 'node:path';
import { mkdir } from 'node:fs/promises';
import { larkSdkHandshakePatch } from './src/lark-sdk-handshake-patch.mjs';
import { wsSdkPatch } from './src/ws-sdk-patch.mjs';

if (Bun.version !== '1.4.2') throw new Error(`此构建要求 Bun 1.4.2，当前为 ${Bun.version}`);
const outfile = resolve(process.argv[2] ?? resolve(import.meta.dir, 'dist/notification-runtime.mjs'));
if (basename(outfile) !== 'notification-runtime.mjs') throw new Error('构建输出必须命名为 notification-runtime.mjs');
const result = await Bun.build({
  entrypoints: [resolve(import.meta.dir, 'src/main.mjs')],
  target: 'bun',
  minify: true,
  metafile: true,
  plugins: [wsSdkPatch, larkSdkHandshakePatch],
});
if (!result.success) throw new Error(result.logs.map((log) => log.message).join('\n'));
if (!Object.keys(result.metafile.inputs).some((path) => path.endsWith('/ws/lib/websocket.js'))) throw new Error('SDK WebSocket 实现未打入 bundle');
await mkdir(dirname(outfile), { recursive: true });
await Bun.write(outfile, result.outputs[0]);
console.log(outfile);
