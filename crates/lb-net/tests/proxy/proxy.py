#!/usr/bin/env python3
"""Loopback-only test proxies that log what the client sends.

  python3 proxy.py --http PORT [--socks PORT] [--auth user:pass] [--log FILE]

- HTTP proxy: `CONNECT host:port` tunnels and absolute-form forward requests
  (`GET http://host/path`). With --auth, requests without the right
  `Proxy-Authorization: Basic` get `407` with `Proxy-Authenticate`.
- SOCKS5 (RFC 1928): no auth, or username/password (RFC 1929) with --auth.

Every target host, whatever its name, is reached at 127.0.0.1 (so names
that do not resolve prove the client left DNS to the proxy). Nothing ever
connects anywhere but loopback. One JSON line per event goes to --log (or
stdout): {"proxy": "http"|"socks", "conn": N, ...}.
"""
import argparse
import asyncio
import base64
import json
import sys

LOOPBACK = "127.0.0.1"
ARGS = None
CONN = 0
OUT = sys.stdout


def log(**kw):
    OUT.write(json.dumps(kw) + "\n")
    OUT.flush()


async def pipe(r, w):
    try:
        while True:
            b = await r.read(65536)
            if not b:
                break
            w.write(b)
            await w.drain()
    except (ConnectionError, asyncio.IncompleteReadError):
        pass
    finally:
        try:
            w.close()
        except Exception:
            pass


async def tunnel(cr, cw, port):
    ur, uw = await asyncio.open_connection(LOOPBACK, port)
    await asyncio.gather(pipe(cr, uw), pipe(ur, cw))


def authorized(headers):
    if not ARGS.auth:
        return True
    want = "Basic " + base64.b64encode(ARGS.auth.encode()).decode()
    return any(k.lower() == "proxy-authorization" and v == want for k, v in headers)


async def http_conn(cr, cw):
    global CONN
    CONN += 1
    me = CONN
    n = 0
    try:
        while True:
            head = await cr.readuntil(b"\r\n\r\n")
            n += 1
            lines = head.decode("latin-1").split("\r\n")[:-2]
            line, headers = lines[0], [tuple(l.split(": ", 1)) for l in lines[1:]]
            log(proxy="http", conn=me, req=n, line=line, headers=headers)
            method, target, _ = line.split(" ", 2)
            length = int(next((v for k, v in headers if k.lower() == "content-length"), "0"))
            body = await cr.readexactly(length) if length else b""
            if not authorized(headers):
                cw.write(b"HTTP/1.1 407 Proxy Authentication Required\r\n"
                         b"Proxy-Authenticate: Basic realm=\"lbtest\"\r\n"
                         b"Content-Length: 0\r\n\r\n")
                await cw.drain()
                continue
            if method == "CONNECT":
                port = int(target.rsplit(":", 1)[1])
                cw.write(b"HTTP/1.1 200 Connection established\r\n\r\n")
                await cw.drain()
                await tunnel(cr, cw, port)
                return
            # Absolute-form forward request: origin-form upstream.
            rest = target.split("://", 1)[1]
            hostport, _, path = rest.partition("/")
            port = int(hostport.rsplit(":", 1)[1]) if ":" in hostport.lstrip("[") else 80
            out = [f"{method} /{path} HTTP/1.1"]
            for k, v in headers:
                if k.lower() in ("proxy-connection", "proxy-authorization", "connection"):
                    continue
                out.append(f"{k}: {v}")
            out.append("Connection: close")
            ur, uw = await asyncio.open_connection(LOOPBACK, port)
            uw.write(("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + body)
            await uw.drain()
            resp = await ur.read()
            uw.close()
            cw.write(resp)
            await cw.drain()
            cw.close()
            return
    except (asyncio.IncompleteReadError, ConnectionError, ValueError):
        pass
    finally:
        try:
            cw.close()
        except Exception:
            pass


async def socks_conn(cr, cw):
    global CONN
    CONN += 1
    me = CONN
    try:
        ver, nm = await cr.readexactly(2)
        methods = list(await cr.readexactly(nm))
        log(proxy="socks", conn=me, greeting=[ver, nm] + methods)
        if ARGS.auth:
            if 2 not in methods:
                cw.write(b"\x05\xff")
                return
            cw.write(b"\x05\x02")
            await cw.drain()
            v, ul = await cr.readexactly(2)
            user = (await cr.readexactly(ul)).decode()
            (pl,) = await cr.readexactly(1)
            pw = (await cr.readexactly(pl)).decode()
            ok = f"{user}:{pw}" == ARGS.auth
            log(proxy="socks", conn=me, auth=[v, user, pw], ok=ok)
            cw.write(b"\x01\x00" if ok else b"\x01\x01")
            await cw.drain()
            if not ok:
                return
        else:
            cw.write(b"\x05\x00")
            await cw.drain()
        ver, cmd, rsv, atyp = await cr.readexactly(4)
        if atyp == 3:
            (ln,) = await cr.readexactly(1)
            addr = (await cr.readexactly(ln)).decode()
        elif atyp == 1:
            addr = ".".join(str(b) for b in await cr.readexactly(4))
        else:
            addr = (await cr.readexactly(16)).hex()
        port = int.from_bytes(await cr.readexactly(2), "big")
        log(proxy="socks", conn=me, request=[ver, cmd, rsv, atyp], host=addr, port=port)
        cw.write(b"\x05\x00\x00\x01\x7f\x00\x00\x01" + port.to_bytes(2, "big"))
        await cw.drain()
        await tunnel(cr, cw, port)
    except (asyncio.IncompleteReadError, ConnectionError):
        pass
    finally:
        try:
            cw.close()
        except Exception:
            pass


async def main():
    servers = []
    if ARGS.http is not None:
        servers.append(await asyncio.start_server(http_conn, LOOPBACK, ARGS.http))
    if ARGS.socks is not None:
        servers.append(await asyncio.start_server(socks_conn, LOOPBACK, ARGS.socks))
    ports = [s.sockets[0].getsockname()[1] for s in servers]
    print("ready", *ports, file=sys.stderr, flush=True)
    await asyncio.gather(*(s.serve_forever() for s in servers))


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--http", type=int)
    p.add_argument("--socks", type=int)
    p.add_argument("--auth")
    p.add_argument("--log")
    ARGS = p.parse_args()
    if ARGS.log:
        OUT = open(ARGS.log, "a")
    asyncio.run(main())
