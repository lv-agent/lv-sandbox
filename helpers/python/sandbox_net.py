#!/usr/bin/env python3
"""cr-019: sandbox 受控出站 helper。

读环境变量 SANDBOX_PROXY_SOCK(SOCKS5h over UDS 代理),把 HTTP 请求经代理转发。
纯标准库,零三方依赖,适合被沙箱化任务 import。
"""
import os
import socket
import ssl
import sys
from urllib.parse import urlparse


def _socks5h_connect(proxy_sock: str, host: str, port: int) -> socket.socket:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(proxy_sock)
    # 问候:NO-AUTH
    s.sendall(b"\x05\x01\x00")
    assert s.recv(2) == b"\x05\x00", "代理拒绝 NO-AUTH"
    # 请求 CONNECT(DOMAIN ATYP,远程 DNS)
    hb = host.encode()
    s.sendall(b"\x05\x01\x00\x03" + bytes([len(hb)]) + hb + port.to_bytes(2, "big"))
    rep = s.recv(10)
    if len(rep) < 2 or rep[1] != 0:
        raise ConnectionError(f"SOCKS5 拒绝(reply={rep[1] if len(rep) > 1 else '?'})")
    return s


class Response:
    def __init__(self, status: int, headers: str, body: bytes):
        self.status = status
        self.headers = headers
        self.body = body

    def __repr__(self):
        return f"<Response {self.status}>"


def request(method: str, url: str, body=None, headers=None) -> Response:
    u = urlparse(url)
    host = u.hostname
    port = u.port or (443 if u.scheme == "https" else 80)
    path = u.path or "/"
    if u.query:
        path += "?" + u.query

    proxy = os.environ.get("SANDBOX_PROXY_SOCK")
    if not proxy:
        raise RuntimeError("SANDBOX_PROXY_SOCK 未设置:该 profile 无出站白名单")

    s = _socks5h_connect(proxy, host, port)
    if u.scheme == "https":
        ctx = ssl.create_default_context()
        s = ctx.wrap_socket(s, server_hostname=host)

    hdrs = {"Host": host, "Connection": "close"}
    if headers:
        hdrs.update(headers)
    req_line = f"{method} {path} HTTP/1.1\r\n"
    for k, v in hdrs.items():
        req_line += f"{k}: {v}\r\n"
    req_line += "\r\n"
    s.sendall(req_line.encode() + (body or b""))

    raw = b""
    while True:
        chunk = s.recv(65536)
        if not chunk:
            break
        raw += chunk
    head, _, resp_body = raw.partition(b"\r\n\r\n")
    head_str = head.decode("latin1")
    status_line = head_str.split("\r\n", 1)[0]
    status = int(status_line.split()[1])
    return Response(status, head_str, resp_body)


def get(url: str, headers=None) -> Response:
    return request("GET", url, headers=headers)


if __name__ == "__main__":
    # CLI: python3 sandbox_net.py GET <url>
    if len(sys.argv) >= 3:
        print(get(sys.argv[2]).body.decode("utf-8", "replace"))
