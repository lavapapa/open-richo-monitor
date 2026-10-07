// Text-only adaptation of dsh-im 4.36.1 src/channels/weixin/weixin-api.mjs,
// which follows Tencent's Weixin iLink API. See NOTICE.md and LICENSE.dsh-im.
import { randomBytes, randomUUID } from 'node:crypto';

export const WEIXIN_BASE_URL = 'https://ilinkai.weixin.qq.com/';
const BASE_INFO = { channel_version: '2.4.6', bot_agent: 'RicohMonitor/1.0' };
const trustedHost = (host) => host === 'weixin.qq.com' || host.endsWith('.weixin.qq.com') || host === 'wechat.com' || host.endsWith('.wechat.com');

function normalizeBase(value = WEIXIN_BASE_URL) {
  const url = new URL(value.includes('://') ? value : `https://${value}`);
  if (url.protocol !== 'https:' || !trustedHost(url.hostname) || (url.port && url.port !== '443')) throw new TypeError('微信连接地址无效');
  url.username = ''; url.password = ''; url.search = ''; url.hash = '';
  if (!url.pathname.endsWith('/')) url.pathname += '/';
  return url;
}

function commonHeaders(token, post) {
  const headers = { 'iLink-App-Id': 'bot', 'iLink-App-ClientVersion': String((2 << 16) | (4 << 8) | 6) };
  if (post) Object.assign(headers, {
    'content-type': 'application/json', AuthorizationType: 'ilink_bot_token',
    'X-WECHAT-UIN': Buffer.from(String(randomBytes(4).readUInt32BE(0)), 'utf8').toString('base64'),
  });
  if (token) headers.Authorization = `Bearer ${token}`;
  return headers;
}

async function request(fetchImpl, base, endpoint, { token, body, signal, timeoutMs = 15_000 } = {}) {
  const timeout = AbortSignal.timeout(timeoutMs);
  const signalOut = signal ? AbortSignal.any([signal, timeout]) : timeout;
  const response = await fetchImpl(new URL(endpoint, normalizeBase(base)), {
    method: body === undefined ? 'GET' : 'POST', headers: commonHeaders(token, body !== undefined),
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: signalOut,
  });
  if (!response.ok) throw Object.assign(new Error(`微信请求失败（HTTP ${response.status}）`), { status: response.status });
  return response.json();
}

export function createWeixinTextApi(fetchImpl = globalThis.fetch) {
  return {
    async beginLogin({ signal } = {}) {
      const value = await request(fetchImpl, WEIXIN_BASE_URL, 'ilink/bot/get_bot_qrcode?bot_type=3', { body: { local_token_list: [] }, signal, timeoutMs: 10_000 });
      if (!value.qrcode || !value.qrcode_img_content) throw new Error('微信没有返回二维码');
      const qr = new URL(value.qrcode_img_content);
      if (qr.protocol !== 'https:' || !trustedHost(qr.hostname)) throw new Error('微信二维码地址无效');
      return { qrcode: value.qrcode, qrUrl: qr.href };
    },
    pollLogin({ qrcode, baseUrl = WEIXIN_BASE_URL, verifyCode, signal } = {}) {
      return request(fetchImpl, baseUrl, `ilink/bot/get_qrcode_status?qrcode=${encodeURIComponent(qrcode)}${verifyCode ? `&verify_code=${encodeURIComponent(verifyCode)}` : ''}`, { signal, timeoutMs: 35_000 });
    },
    notifyStart({ baseUrl, token, signal }) {
      return request(fetchImpl, baseUrl, 'ilink/bot/msg/notifystart', { token, body: { base_info: BASE_INFO }, signal, timeoutMs: 10_000 });
    },
    notifyStop({ baseUrl, token }) {
      return request(fetchImpl, baseUrl, 'ilink/bot/msg/notifystop', { token, body: { base_info: BASE_INFO }, timeoutMs: 1500 });
    },
    async getUpdates({ baseUrl, token, cursor = '', signal } = {}) {
      try {
        return await request(fetchImpl, baseUrl, 'ilink/bot/getupdates', { token, body: { get_updates_buf: cursor, base_info: BASE_INFO }, signal, timeoutMs: 35_000 });
      } catch (error) {
        if (error?.name === 'TimeoutError' && !signal?.aborted) return { ret: 0, msgs: [], get_updates_buf: cursor };
        throw error;
      }
    },
    sendText({ baseUrl, token, targetId, text, contextToken, signal }) {
      return request(fetchImpl, baseUrl, 'ilink/bot/sendmessage', {
        token, signal,
        body: { msg: {
          from_user_id: '', to_user_id: targetId, client_id: `ricoh-${randomUUID()}`,
          message_type: 2, message_state: 2,
          item_list: [{ type: 1, text_item: { text } }],
          ...(contextToken ? { context_token: contextToken } : {}),
        }, base_info: BASE_INFO },
      });
    },
  };
}
