#!/usr/bin/env node
import readline from 'node:readline';

const accounts = new Map();
const bindings = new Map();
const sendCounts = new Map();
const recipientCounts = new Map();
let configured = 0;
let lastConfiguration;
let lastBindingRequest;
let ignoreEof = false;
let ignoreStatus = false;
const write = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const lines = readline.createInterface({ input: process.stdin });
for await (const line of lines) {
  void handle(line);
}
async function handle(line) {
  const { id, method, params } = JSON.parse(line);
  let result = {};
  if (method === 'configure') {
    configured += 1;
    lastConfiguration = params;
    ignoreStatus = params.accounts.some(account => account.credentials.ignoreStatus);
    accounts.clear();
    for (const account of params.accounts) {
      accounts.set(account.id, account);
      ignoreEof ||= !!account.credentials.ignoreEof;
      write({ event: 'account', data: { id: account.id, status: account.enabled ? (account.credentials.connectionStatus ?? (account.credentials.connectDelayMs ? 'connecting' : 'ready')) : 'stopped', targets: account.targets } });
      if (account.enabled && account.credentials.connectDelayMs) setTimeout(() => {
        if (accounts.get(account.id)?.enabled) write({ event: 'account', data: { id: account.id, status: 'ready', targets: account.targets } });
      }, account.credentials.connectDelayMs);
    }
  } else if (method === 'begin_binding') {
    lastBindingRequest = params;
    result = { id: params.bindingId, provider: params.provider, status: 'waiting', qrUrl: 'https://example.invalid/qr', targets: [] };
    bindings.set(result.id, result);
    write({ event: 'binding', data: result });
  } else if (method === 'binding_status') {
    const existingTargets = bindings.get(params.bindingId)?.targets;
    result = { ...bindings.get(params.bindingId), status: 'complete', credentials: { appId: 'bound-app', appSecret: 'private-bound-secret' }, targets: existingTargets?.length ? existingTargets : [{ id: 'chat-a', kind: 'chat', label: '会话 A' }, { id: 'user-b', kind: 'user', label: '用户 B' }] };
    bindings.set(result.id, result);
    const { credentials, ...publicBinding } = result;
    write({ event: 'binding', data: publicBinding });
  } else if (method === 'cancel_binding') {
    bindings.delete(params.bindingId);
  } else if (method === 'send') {
    const account = accounts.get(params.accountId);
    sendCounts.set(params.accountId, (sendCounts.get(params.accountId) ?? 0) + 1);
    const recipient = `${params.accountId}:${params.target.kind}:${params.target.id}`;
    recipientCounts.set(recipient,(recipientCounts.get(recipient) ?? 0) + 1);
    if (account?.credentials.dropConnection) process.exit(0);
    if (account?.credentials.delayMs) await new Promise((resolve) => setTimeout(resolve, account.credentials.delayMs));
    if (account?.credentials.updateContext) write({ event: 'credentials', data: { accountId: params.accountId, credentials: { contextTokens: { ...account.credentials.contextTokens, [params.target.id]: 'fresh-private-context' } } } });
    result = { outcome: account?.credentials.recipientOutcomes?.[params.target.id] ?? account?.credentials.fixtureOutcome ?? 'accepted', message: account?.credentials.recipientMessages?.[params.target.id] ?? 'fixture result', retryable: !!account?.credentials.retryable };
  } else if (method === 'detect_groups' || method === 'detect_binding_groups') {
    result = { targets: [{id:'chat-a',kind:'chat',label:'群组 A'},{id:'chat-b',kind:'chat',label:'群组 B'}] };
  } else if (method === 'fixture_target_alias') {
    write({ event: 'credentials', data: { accountId: params.accountId, credentials: { targetAliases: params.aliases } } });
  } else if (method === 'fixture_target_name') {
    write({ event: 'target', data: { accountId: params.accountId, target: params.target } });
  } else if (method === 'fixture_context_update') {
    write({ event: 'credentials', data: { accountId: params.accountId, credentials: { contextTokens: { owner: 'old-session-context' }, contextMetadata: { owner: { seq: '99' } } } } });
  } else if (method === 'status') {
    if (ignoreStatus) return;
    result = { pid: process.pid, configured, sendCounts: Object.fromEntries(sendCounts),recipientCounts:Object.fromEntries(recipientCounts), configuration: lastConfiguration, lastBindingRequest };
  } else if (method === 'hang') {
    await new Promise(() => {});
  } else if (method === 'reject') {
    write({ id, error: { code: -32001, message: 'private-bound-secret' } });
    return;
  }
  write({ id, result });
}
if (ignoreEof) setInterval(() => {}, 1000);
