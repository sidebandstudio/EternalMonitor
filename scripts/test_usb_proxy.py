import asyncio
import unittest

from usb_proxy import USBProxy


class CableFixture(unittest.IsolatedAsyncioTestCase):
    async def test_unplug_closes_an_active_tunnel_before_waiting_for_server(self):
        async def echo(reader, writer):
            try:
                while data := await reader.read(4096):
                    writer.write(data)
                    await writer.drain()
            finally:
                writer.close()

        target = await asyncio.start_server(echo, "127.0.0.1", 0)
        proxy = USBProxy(0, target.sockets[0].getsockname()[1])
        try:
            # Replug must accept a fresh connection after the first EOF.
            for _ in range(2):
                await proxy.start()
                port = proxy.server.sockets[0].getsockname()[1]
                reader, writer = await asyncio.open_connection("127.0.0.1", port)
                try:
                    writer.write(b"EMLINK\x01\x00")
                    await writer.drain()
                    self.assertEqual(await asyncio.wait_for(reader.readexactly(8), 1), b"EMLINK\x01\x00")
                    await asyncio.wait_for(proxy.stop(), 1)
                    self.assertEqual(await asyncio.wait_for(reader.read(1), 1), b"")
                finally:
                    writer.close()
                    await writer.wait_closed()
        finally:
            await proxy.stop()
            target.close()
            await target.wait_closed()


if __name__ == "__main__":
    unittest.main()
