const url = 'https://open.feishu.cn/';
try {
  const response = await fetch(url, { signal: AbortSignal.timeout(5_000) });
  console.log(JSON.stringify({ ok: response.ok, status: response.status, url: new URL(response.url).origin }));
} catch (error) {
  console.log(JSON.stringify({ name: error?.name, message: String(error?.message).slice(0, 120) }));
}
