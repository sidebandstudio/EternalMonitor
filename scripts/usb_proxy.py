#!/usr/bin/env python3
"""Expose a loopback USB tunnel on demand for takeover and unplug tests."""
import argparse
import asyncio
import signal


class USBProxy:
    def __init__(self, port, target):
        self.port = port
        self.target = target
        self.server = None
        self.writers = set()

    async def start(self):
        if self.server is None:
            self.server = await asyncio.start_server(self.forward, "127.0.0.1", self.port)

    async def stop(self):
        server, self.server = self.server, None
        if server:
            server.close()
        for writer in list(self.writers):
            writer.close()
        # Newer asyncio versions wait for accepted clients too. Close those
        # before waiting, otherwise an unplug command waits for its own EOF.
        if server:
            await server.wait_closed()

    async def forward(self, reader, writer):
        upstream = None
        self.writers.add(writer)
        try:
            source, upstream = await asyncio.open_connection("127.0.0.1", self.target)
            self.writers.add(upstream)

            async def copy(read, write):
                try:
                    while data := await read.read(16 * 1024):
                        write.write(data)
                        await write.drain()
                finally:
                    write.close()

            await asyncio.gather(copy(reader, upstream), copy(source, writer))
        except (ConnectionError, OSError):
            pass
        finally:
            for stream in [writer, upstream]:
                if stream:
                    self.writers.discard(stream)
                    stream.close()

    async def control(self, reader, writer):
        try:
            command = await asyncio.wait_for(reader.readline(), timeout=2)
            if command == b"START\n":
                await self.start()
            elif command == b"STOP\n":
                await self.stop()
            else:
                raise ValueError("expected START or STOP")
            writer.write(b"OK\n")
            await writer.drain()
        except (ValueError, ConnectionError, OSError, asyncio.TimeoutError):
            writer.write(b"ERROR\n")
        finally:
            writer.close()


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=19873)
    parser.add_argument("--target", type=int, default=9877)
    parser.add_argument("--control-port", type=int)
    args = parser.parse_args()
    proxy = USBProxy(args.port, args.target)
    control = None
    if args.control_port:
        control = await asyncio.start_server(proxy.control, "127.0.0.1", args.control_port)
    else:
        await proxy.start()
    print("USB proxy ready", flush=True)
    stopped = asyncio.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        asyncio.get_running_loop().add_signal_handler(sig, stopped.set)
    try:
        await stopped.wait()
    finally:
        await proxy.stop()
        if control:
            control.close()
            await control.wait_closed()


if __name__ == "__main__":
    asyncio.run(main())
