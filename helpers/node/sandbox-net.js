#!/usr/bin/env node
// cr-019: sandbox 受控出站 helper(SOCKS5h over UDS + HTTP/HTTPS)。
// 纯 Node 内置模块,零三方依赖。

const net = require("net");
const tls = require("tls");
const http = require("http");
const { URL } = require("url");

function socks5hConnect(proxyPath, host, port, cb) {
  const s = net.createConnection(proxyPath);
  s.once("connect", () => {
    s.write(Buffer.from([0x05, 0x01, 0x00])); // VER, NMETHODS=1, NO-AUTH
  });
  s.once("data", (greet) => {
    if (greet[0] !== 0x05 || greet[1] !== 0x00) return cb(new Error("proxy auth failed"));
    const hb = Buffer.from(host);
    const req = Buffer.concat([
      Buffer.from([0x05, 0x01, 0x00, 0x03, hb.length]), hb,
      Buffer.from([port >> 8, port & 0xff]),
    ]);
    s.write(req);
  });
  let once = false;
  s.on("data", (rep) => {
    if (once) return;
    once = true;
    if (rep[1] !== 0x00) return cb(new Error("socks5 reply " + rep[1]));
    cb(null, s);
  });
  s.on("error", (e) => cb(e));
}

function request(method, urlStr, body, headers, cb) {
  const u = new URL(urlStr);
  const host = u.hostname;
  const port = Number(u.port) || (u.protocol === "https:" ? 443 : 80);
  const proxy = process.env.SANDBOX_PROXY_SOCK;
  if (!proxy) return cb(new Error("SANDBOX_PROXY_SOCK 未设置"));
  socks5hConnect(proxy, host, port, (err, sock) => {
    if (err) return cb(err);
    const opts = { method, host: "127.0.0.1", path: u.pathname + u.search, headers: headers || {} };
    opts.headers.Host = host;
    opts.headers.Connection = "close";
    opts.createConnection = () =>
      u.protocol === "https:" ? tls.connect({ socket: sock, servername: host }) : sock;
    const req = http.request(opts, (res) => {
      let chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () =>
        cb(null, { status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) })
      );
    });
    req.on("error", cb);
    if (body) req.write(body);
    req.end();
  });
}

module.exports = { request, get: (u, cb) => request("GET", u, null, null, cb) };

if (require.main === module) {
  const [, , m, urlStr] = process.argv;
  request(m || "GET", urlStr, null, null, (err, r) => {
    if (err) { console.error(err.message); process.exit(1); }
    process.stdout.write(r.body);
  });
}
