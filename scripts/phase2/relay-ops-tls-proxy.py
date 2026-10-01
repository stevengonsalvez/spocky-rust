#!/usr/bin/env python3
"""Disposable TLS edge used to prove the relay remains cleartext behind TLS."""

import argparse
import selectors
import socket
import socketserver
import ssl


class ProxyHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        upstream = socket.create_connection(self.server.upstream, timeout=5)
        selector = selectors.DefaultSelector()
        selector.register(self.request, selectors.EVENT_READ, upstream)
        selector.register(upstream, selectors.EVENT_READ, self.request)
        try:
            while True:
                events = selector.select(timeout=5)
                if not events:
                    continue
                for key, _mask in events:
                    data = key.fileobj.recv(65536)
                    if not data:
                        return
                    key.data.sendall(data)
        finally:
            selector.close()
            upstream.close()


class ThreadingProxy(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen-host", required=True)
    parser.add_argument("--listen-port", required=True, type=int)
    parser.add_argument("--upstream-host", required=True)
    parser.add_argument("--upstream-port", required=True, type=int)
    parser.add_argument("--cert", required=True)
    parser.add_argument("--key", required=True)
    args = parser.parse_args()

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.cert, args.key)
    with ThreadingProxy((args.listen_host, args.listen_port), ProxyHandler) as server:
        server.upstream = (args.upstream_host, args.upstream_port)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        server.serve_forever(poll_interval=0.1)


if __name__ == "__main__":
    main()
