import { execFile } from 'node:child_process';
import http from 'node:http';
import https from 'node:https';
import Agent from 'agent-base';
import { parse } from 'node:url';
import HttpsProxyAgent from 'https-proxy-agent';

export const WINDOWS_SYSTEM_PROXY = 'windows-system://current';

function helperArgument() {
  const index = process.argv.indexOf('--system-proxy-helper');
  return index >= 0 ? process.argv[index + 1] : undefined;
}

export function nativeProxyResolver(helper = helperArgument()) {
  const cache = new Map();
  return async (url, signal) => {
    if (!helper) throw new Error('Windows 系统代理解析程序不可用');
    signal?.throwIfAborted();
    const saved = cache.get(url);
    if (saved?.expires > Date.now()) return saved.proxyUrl;
    const value = await new Promise((resolve, reject) => {
      execFile(helper, ['--rm-resolve-system-proxy', url], {
        windowsHide: true, timeout: 10_000, maxBuffer: 8_192, signal,
      }, (error, stdout) => {
        if (error) return reject(error);
        try {
          const result = JSON.parse(stdout);
          if (result.error) throw new Error(result.error);
          resolve(result.proxyUrl);
        } catch (cause) { reject(cause); }
      });
    });
    if (cache.size >= 100) cache.delete(cache.keys().next().value);
    cache.set(url, { proxyUrl: value, expires: Date.now() + 30_000 });
    return value;
  };
}

export function systemProxyAgent(resolve, signal) {
  return new Agent(async (request, options) => {
    const protocol = options.secureEndpoint ? 'https:' : 'http:';
    const target = new URL(request.path, `${protocol}//${request.getHeader('host')}`).href;
    const proxy = await resolve(target, signal);
    signal?.throwIfAborted();
    if (!proxy) return options.secureEndpoint ? https.globalAgent : http.globalAgent;
    return new HttpsProxyAgent.HttpsProxyAgent({ ...parse(proxy), signal }).callback(request, options);
  });
}
