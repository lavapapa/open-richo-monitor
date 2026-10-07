// Adapted from dsh-im 4.36.1 plugin-src/host/lark-sdk-handshake-patch.mjs.
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const npmWs = fileURLToPath(new URL('../node_modules/ws/wrapper.mjs', import.meta.url));

const patches = [
  [`        this.wsConfig = new WSConfig();\n        this.reconnectGeneration = 0;\n        this.isConnecting = false;`, `        this.wsConfig = new WSConfig();\n        this.reconnectGeneration = 0;\n        this.pendingWsInstance = null;\n        this.isConnecting = false;`],
  [`        if (!wsInstance) {\n            return Promise.resolve(false);\n        }\n        return new Promise((resolve) => {`, `        if (!wsInstance) {\n            return Promise.resolve(false);\n        }\n        this.pendingWsInstance = wsInstance;\n        return new Promise((resolve) => {`],
  [`                if (timer)\n                    clearTimeout(timer);\n                resolve(ok);`, `                if (timer)\n                    clearTimeout(timer);\n                if (this.pendingWsInstance === wsInstance)\n                    this.pendingWsInstance = null;\n                resolve(ok);`],
  [`                    this.logger.error('[ws]', \`handshake timeout after \${this.handshakeTimeoutMs}ms\`);\n                    wsInstance.removeAllListeners();`, `                    this.logger.error('[ws]', \`handshake timeout after \${this.handshakeTimeoutMs}ms\`);\n                    wsInstance.removeAllListeners('open');`],
  [`            const tryConnect = () => __awaiter(this, void 0, void 0, function* () {\n                this.reconnectInfo.lastConnectTime = Date.now();\n                const pullResult = yield this.pullConnectConfig();\n                if (!pullResult.ok)\n                    return pullResult;\n                const connected = yield this.connect();\n                if (!connected)\n                    return { ok: false, retryable: true };\n                this.communicate();\n                return { ok: true };\n            });`, `            const tryConnect = () => __awaiter(this, void 0, void 0, function* () {\n                if (currentGeneration !== this.reconnectGeneration)\n                    return { ok: false, retryable: false, cancelled: true };\n                this.reconnectInfo.lastConnectTime = Date.now();\n                const pullResult = yield this.pullConnectConfig();\n                if (currentGeneration !== this.reconnectGeneration)\n                    return { ok: false, retryable: false, cancelled: true };\n                if (!pullResult.ok)\n                    return pullResult;\n                const connected = yield this.connect();\n                if (currentGeneration !== this.reconnectGeneration)\n                    return { ok: false, retryable: false, cancelled: true };\n                if (!connected)\n                    return { ok: false, retryable: true };\n                this.communicate();\n                return { ok: true };\n            });`],
  [`                try {\n                    result = yield tryConnect();\n                }\n                finally {\n                    this.isConnecting = false;\n                }\n                if (result.ok) {`, `                try {\n                    result = yield tryConnect();\n                }\n                finally {\n                    if (currentGeneration === this.reconnectGeneration) {\n                        this.isConnecting = false;\n                    }\n                }\n                if (currentGeneration !== this.reconnectGeneration || result.cancelled) {\n                    return;\n                }\n                if (result.ok) {`],
  [`        const wsInstance = this.wsConfig.getWSInstance();\n        if (wsInstance) {`, `        const pendingWsInstance = this.pendingWsInstance;\n        if (pendingWsInstance) {\n            this.pendingWsInstance = null;\n            pendingWsInstance.removeAllListeners('open');\n            try {\n                if (force) {\n                    pendingWsInstance.terminate();\n                }\n                else {\n                    pendingWsInstance.close();\n                }\n            }\n            catch (_error) { /* closing a socket is best effort */ }\n        }\n        const wsInstance = this.wsConfig.getWSInstance();\n        if (wsInstance) {`],
  [`            const { eventDispatcher } = params;\n            if (!eventDispatcher) {\n                this.logger.warn('[ws]', 'client need to start with a eventDispatcher');\n                return;\n            }\n            // Clear any terminal-error state left over from a previous session so`, `            const { eventDispatcher } = params;\n            if (!eventDispatcher) {\n                this.logger.warn('[ws]', 'client need to start with a eventDispatcher');\n                return;\n            }\n            const liveWsInstance = this.wsConfig.getWSInstance();\n            if (this.terminalError) {\n                this.isConnecting = false;\n            }\n            if (this.isConnecting ||\n                (liveWsInstance && liveWsInstance.readyState !== WebSocket.CLOSED)) {\n                this.logger.debug('[ws]', 'start ignored because client is already connecting or connected');\n                return;\n            }\n            // Clear any terminal-error state left over from a previous session so`],
];

export function patchLarkSdkHandshakeSource(source, path = 'Lark SDK') {
  return patches.reduce((result, [before, after], index) => {
    if (result.split(before).length !== 2) throw new Error(`${path}: reviewed SDK fragment ${index + 1} no longer matches exactly once`);
    return result.replace(before, after);
  }, source);
}

export const larkSdkHandshakePatch = {
  name: 'dsh-lark-sdk-websocket-lifecycle-fix',
  setup(build) {
    build.onLoad({ filter: /@larksuiteoapi[\\/]node-sdk[\\/](es|lib)[\\/]index\.js$/ }, async ({ path }) => ({
      contents: patchLarkSdkHandshakeSource(await readFile(path, 'utf8'), path).replace(/from ['"]ws['"]/g, `from ${JSON.stringify(npmWs)}`), loader: 'js',
    }));
  },
};
