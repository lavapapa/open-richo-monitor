export default {
  async fetch(request, env) {
    if (new URL(request.url).pathname !== '/feedback') return new Response('Not found', { status: 404 });
    if (request.method !== 'POST') return new Response('Method not allowed', { status: 405, headers: { Allow: 'POST' } });
    let body;
    try { body = await request.json(); } catch { return Response.json({ ok: false }, { status: 400 }); }
    if (typeof body?.message !== 'string' || !body.message.trim() || [...body.message].length > 4000) return Response.json({ ok: false }, { status: 400 });
    const record = { message: body.message.trim(), version: typeof body.version === 'string' ? body.version.slice(0, 80) : null, platform: typeof body.platform === 'string' ? body.platform.slice(0, 40) : null, receivedAt: new Date().toISOString() };
    try {
      await env.FEEDBACK.put(`${record.receivedAt}/${crypto.randomUUID()}`, JSON.stringify(record));
    } catch {
      return Response.json({ ok: false }, { status: 503 });
    }
    return Response.json({ ok: true });
  },
};
