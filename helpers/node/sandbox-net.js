#!/usr/bin/env node
// cr-019: sandbox 受控出站 helper(SOCKS5h over UDS + 原始 HTTP/HTTPS)。
// 纯 Node 内置模块,零三方依赖。手法对齐 python helper:SOCKS5h 握手后在
// relay 流上手写 HTTP,避免 http.agent 复用已连接 socket 的关闭问题。

const net = require("net");
const tls = require("tls");
const { URL } = require("url");

function socks5hConnect(proxyPath, host, port, cb) {
  const s = net.createConnection(proxyPath);
  let state = 0; // 0=等问候回复, 1=等 CONNECT 回复
  s.once("connect", () => {
    s.write(Buffer.from([0x05, 0x01, 0x00])); // VER, NMETHODS=1, NO-AUTH
  });
  s.on("data", (chunk) => {
    if (state === 0) {
      if (chunk[0] !== 0x05 || chunk[1] !== 0x00) {
        s.destroy();
        return cb(new Error("proxy auth failed"));
      }
      const hb = Buffer.from(host);
      const req = Buffer.concat([
        Buffer.from([0x05, 0x01, 0x00, 0x03, hb.length]), hb,
        Buffer.from([port >> 8, port & 0xff]),
      ]);
      s.write(req);
      state = 1;
    } else if (state === 1) {
      if (chunk[1] !== 0x00) {
        s.destroy();
        return cb(new Error("socks5 reply " + chunk[1]));
      }
      // 关键:移除握手监听,让后续 HTTP 数据流由 rawHttp 接管
      s.removeAllListeners("data");
      cb(null, s);
    }
  });
  s.on("error", (e) => cb(e));
}

// 在已就绪的流上写原始 HTTP 请求,收集响应直到 EOF,解析状态码 + body。
function rawHttp(stream, requestStr, cb) {
  const chunks = [];
  stream.on("data", (c) => chunks.push(c));
  stream.on("end", () => {
    const raw = Buffer.concat(chunks);
    const sep = raw.indexOf("\r\n\r\n");
    const head = (sep >= 0 ? raw.slice(0, sep) : raw).toString("latin1");
    const statusLine = head.split("\r\n")[0];
    const status = parseInt(statusLine.split(" ")[1], 10);
    cb(null, { status, headers: head, body: sep >= 0 ? raw.slice(sep + 4) : Buffer.alloc(0) });
  });
  stream.on("error", (e) => cb(e));
  stream.write(requestStr);
}

function request(method, urlStr, body, headers, cb) {
  const u = new URL(urlStr);
  const host = u.hostname;
  const port = Number(u.port) || (u.protocol === "https:" ? 443 : 80);
  const path = (u.pathname || "/") + u.search;
  const proxy = process.env.SANDBOX_PROXY_SOCK;
  if (!proxy) return cb(new Error("SANDBOX_PROXY_SOCK 未设置"));

  const lines = [`${method} ${path} HTTP/1.1`, `Host: ${host}`, "Connection: close"];
  if (headers) for (const k in headers) lines.push(`${k}: ${headers[k]}`);
  const requestStr = lines.join("\r\n") + "\r\n\r\n" + (body || "");

  socks5hConnect(proxy, host, port, (err, sock) => {
    if (err) return cb(err);
    if (u.protocol === "https:") {
      const ts = tls.connect({ socket: sock, servername: host }, () => rawHttp(ts, requestStr, cb));
      ts.on("error", (e) => cb(e));
    } else {
      rawHttp(sock, requestStr, cb);
    }
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
