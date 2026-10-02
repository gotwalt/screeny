#!/usr/bin/env python3
"""A stand-in for Home Assistant's ingress, to check the Studio's pages under a
path prefix without a Home Assistant (card 359).

    tools/ingress-sim.py [--listen 127.0.0.1:8801] [--upstream 127.0.0.1:8802]
                         [--prefix /api/hassio_ingress/abc123] [--seconds 300]

It serves `<prefix>/...` by forwarding `/...` to the upstream with an
`X-Ingress-Path` header, as the Supervisor does, and answers 404 to anything
outside the prefix - which is what makes a root-absolute URL in a page show up.
WebSocket upgrades are piped through. Plain asyncio, one connection per
request, and it exits by itself after `--seconds`.

The Studio side, as an app would run (peers = this proxy's address):

    SUPERVISOR_TOKEN=dummy SCREENY_APP_INGRESS_LISTEN=127.0.0.1:8802 \\
    SCREENY_APP_INGRESS_PEERS=127.0.0.1 SCREENY_SUPERVISOR_URL=http://127.0.0.1:1 \\
    SCREENY_STATE_DIR=/tmp/x screeny-studio --no-discover
"""
import argparse
import asyncio


async def pipe(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        try:
            writer.close()
        except Exception:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", default="127.0.0.1:8801")
    ap.add_argument("--upstream", default="127.0.0.1:8802")
    ap.add_argument("--prefix", default="/api/hassio_ingress/abc123")
    ap.add_argument("--seconds", type=int, default=300)
    a = ap.parse_args()
    lh, lp = a.listen.rsplit(":", 1)
    uh, up = a.upstream.rsplit(":", 1)

    async def handle(cr, cw):
        try:
            head = await cr.readuntil(b"\r\n\r\n")
            lines = head.decode("latin1").split("\r\n")
            method, target, version = lines[0].split(" ", 2)
            if not (target == a.prefix or target.startswith(a.prefix + "/") or target.startswith(a.prefix + "?")):
                body = b"outside the ingress prefix"
                cw.write(b"HTTP/1.1 404 Not Found\r\nContent-Length: %d\r\nConnection: close\r\n\r\n" % len(body) + body)
                await cw.drain()
                print("404", method, target, flush=True)
                cw.close()
                return
            path = target[len(a.prefix):] or "/"
            if not path.startswith("/"):
                path = "/" + path
            upgrade = any(l.lower().startswith("upgrade:") for l in lines)
            out = [f"{method} {path} {version}"]
            for l in lines[1:]:
                if not l or l.lower().startswith(("connection:", "x-ingress-path:")):
                    continue
                out.append(l)
            out.append(f"X-Ingress-Path: {a.prefix}")
            out.append("Connection: Upgrade" if upgrade else "Connection: close")
            print("->", method, path, "(upgrade)" if upgrade else "", flush=True)
            ur, uw = await asyncio.open_connection(uh, int(up))
            uw.write(("\r\n".join(out) + "\r\n\r\n").encode("latin1"))
            await uw.drain()
            await asyncio.gather(pipe(cr, uw), pipe(ur, cw))
        except (asyncio.IncompleteReadError, ConnectionError):
            pass
        finally:
            cw.close()

    async def run():
        server = await asyncio.start_server(handle, lh, int(lp))
        print(f"ingress-sim: http://{a.listen}{a.prefix}/ -> {a.upstream}", flush=True)
        async with server:
            try:
                await asyncio.wait_for(server.serve_forever(), a.seconds)
            except asyncio.TimeoutError:
                pass

    asyncio.run(run())


if __name__ == "__main__":
    main()
