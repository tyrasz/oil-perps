// Serves one rebuilt mainnet update as Hermes would, and only with a bearer key
const http = require('http');
const body = require('fs').readFileSync(process.argv[2]);
let n = 0;
http.createServer((req, res) => {
  n++;
  const ok = req.headers.authorization === 'Bearer test-key' && req.url.startsWith('/v2/updates/price/latest');
  console.log(JSON.stringify({ n, url: req.url.slice(0, 120), auth: !!req.headers.authorization, ok }));
  res.writeHead(ok ? 200 : 401, { 'content-type': ok ? 'application/json' : 'text/plain' });
  res.end(ok ? body : 'unauthorized');
}).listen(8788, '127.0.0.1');
