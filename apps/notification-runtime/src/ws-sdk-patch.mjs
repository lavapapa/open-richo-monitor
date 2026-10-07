import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const npmWs = fileURLToPath(new URL('../node_modules/ws/wrapper.mjs', import.meta.url));

export const wsSdkPatch = {
  name: 'resolve-sdk-ws-to-packaged-npm-module',
  setup(build) {
    build.onLoad({ filter: /(?:@wecom[\\/]aibot-node-sdk[\\/]dist[\\/]index\.esm\.js|dingtalk-stream[\\/]dist[\\/]client\.mjs)$/ }, async ({ path }) => ({
      contents: (await readFile(path, 'utf8')).replace(/from ['"]ws['"]/g, `from ${JSON.stringify(npmWs)}`),
      loader: 'js',
    }));
  },
};
