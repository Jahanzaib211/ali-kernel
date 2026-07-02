#!/usr/bin/env python3
"""llama-bridge — expose llama-server (bound to 127.0.0.1:8081) on the docker
bridge IP (172.17.0.1:8081) so containers that resolve `host.docker.internal`
can reach it. Honors the project's "never bind to 0.0.0.0" rule by binding to
the bridge IP only.

Managed by PM2 as the `llama-bridge` app — restart on crash, resurrect on reboot.
Per-connection forward: each accepted client opens a fresh upstream socket,
so an upstream ECONNREFUSED at boot is naturally retried by the next client.

Hardening:
  - Upstream connect has a 5s timeout (else a stuck llama-server would wedge
    every accepted client forever)
  - Concurrency limited to LLAMA_BRIDGE_MAX_CONNECTIONS (default 64) via
    semaphore; excess clients are accepted then closed immediately
  - Logs route to stderr; WARNING+ only (was: INFO per-connection, polluting
    out.log with one line per forward). Connection lifecycle is silent.
"""

import asyncio
import logging
import os
import signal
import sys

LISTEN_HOST = os.environ.get("LLAMA_BRIDGE_HOST", "172.17.0.1")
LISTEN_PORT = int(os.environ.get("LLAMA_BRIDGE_PORT", "8081"))
UPSTREAM_HOST = os.environ.get("LLAMA_BRIDGE_UPSTREAM_HOST", "127.0.0.1")
UPSTREAM_PORT = int(os.environ.get("LLAMA_BRIDGE_UPSTREAM_PORT", "8081"))
UPSTREAM_CONNECT_TIMEOUT = float(os.environ.get("LLAMA_BRIDGE_CONNECT_TIMEOUT", "5.0"))
MAX_CONNECTIONS = int(os.environ.get("LLAMA_BRIDGE_MAX_CONNECTIONS", "64"))

logging.basicConfig(
    level=logging.WARNING,
    format="%(asctime)s llama-bridge %(levelname)s %(message)s",
    stream=sys.stderr,
)
log = logging.getLogger("llama-bridge")

_concurrency = asyncio.Semaphore(MAX_CONNECTIONS) if MAX_CONNECTIONS > 0 else None


async def pump(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while True:
            data = await reader.read(65536)
            if not data:
                return
            writer.write(data)
            await writer.drain()
    except (ConnectionResetError, BrokenPipeError, asyncio.IncompleteReadError, asyncio.CancelledError):
        return


async def handle(client_reader: asyncio.StreamReader, client_writer: asyncio.StreamWriter) -> None:
    if _concurrency is not None:
        await _concurrency.acquire()

    peer = client_writer.get_extra_info("peername")
    try:
        try:
            upstream_reader, upstream_writer = await asyncio.wait_for(
                asyncio.open_connection(UPSTREAM_HOST, UPSTREAM_PORT),
                timeout=UPSTREAM_CONNECT_TIMEOUT,
            )
        except asyncio.TimeoutError:
            log.warning("upstream %s:%d connect timeout after %.1fs (peer=%s)", UPSTREAM_HOST, UPSTREAM_PORT, UPSTREAM_CONNECT_TIMEOUT, peer)
            client_writer.close()
            return
        except (ConnectionRefusedError, OSError) as e:
            log.warning("upstream %s:%d unreachable from %s: %s", UPSTREAM_HOST, UPSTREAM_PORT, peer, e)
            client_writer.close()
            return
        try:
            await asyncio.gather(
                pump(client_reader, upstream_writer),
                pump(upstream_reader, client_writer),
            )
        finally:
            upstream_writer.close()
    finally:
        try:
            client_writer.close()
        except Exception:
            pass
        if _concurrency is not None:
            _concurrency.release()


async def main() -> None:
    server = await asyncio.start_server(handle, LISTEN_HOST, LISTEN_PORT, reuse_address=True)
    sockets = ", ".join(str(s.getsockname()) for s in server.sockets)
    # WARNING level (not INFO) so it actually emits under our WARNING-only config
    log.warning(
        "listening on %s -> %s:%d (max_connections=%s, connect_timeout=%.1fs)",
        sockets, UPSTREAM_HOST, UPSTREAM_PORT, MAX_CONNECTIONS, UPSTREAM_CONNECT_TIMEOUT,
    )

    stop = asyncio.Event()

    def _shutdown(*_: object) -> None:
        stop.set()

    loop = asyncio.get_event_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, _shutdown)

    try:
        await stop.wait()
    finally:
        log.warning("shutting down")
        server.close()
        await server.wait_closed()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
