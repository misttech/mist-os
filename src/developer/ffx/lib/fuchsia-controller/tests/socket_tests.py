# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import unittest

from fuchsia_controller_py import Channel, Socket, ZxStatus

from fidl import AsyncSocket


class SocketTests(unittest.IsolatedAsyncioTestCase):
    """Socket tests."""

    def test_socket_write_then_read(self):
        """Tests a simple write followed by an immediate read."""
        (sock_out, sock_in) = Socket.create()
        bytes_in = bytearray([1, 2, 3])
        sock_out.write(bytes_in)
        bytes_out = sock_in.read()
        self.assertEqual(bytes_out, bytes_in)

    def test_socket_write_fails_when_closed(self):
        """Verifies PEER_CLOSED is surfaced when opposing socket is closed."""
        (sock_out, sock_in) = Socket.create()
        del sock_in
        with self.assertRaises(ZxStatus):
            try:
                sock_out.write(bytearray([1, 2, 3]))
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    def test_socket_passing(self):
        """Verifies a socket remains connected when passed through a channel."""
        (chan_out, chan_in) = Channel.create()
        (sock_out, sock_in) = Socket.create()
        # This is using 'take' rather than 'as_int' as using 'as_int' would cause a double-close
        # error on a channel that has already been closed.
        chan_out.write((bytearray(), [(0, sock_out.take(), 0, 0, 0)]))
        _, hdls = chan_in.read()
        self.assertEqual(len(hdls), 1)
        bytes_in = bytearray([1, 2, 3])
        new_sock_out = Socket(hdls[0])
        new_sock_out.write(bytes_in)
        bytes_out = sock_in.read()
        self.assertEqual(bytes_out, bytes_in)

    async def test_async_socket(self):
        """Verifies an async socket is able to wait for the other end to complete writing."""
        (sock_out, sock_in) = Socket.create()
        sock_in = AsyncSocket(sock_in)
        bytes_in = bytearray([1, 2, 3])

        async def slow_write(sock: Socket, b: bytes) -> None:
            await asyncio.sleep(1)
            sock.write(b)

        loop = asyncio.get_running_loop()
        write_task = loop.create_task(slow_write(sock_out, bytes_in))
        read_task = loop.create_task(sock_in.read())
        done, _ = await asyncio.wait([write_task, read_task])
        self.assertEqual(len(done), 2)
        read_bytes = None
        for item in done:
            if item.result() is not None:
                read_bytes = item.result()
        self.assertEqual(read_bytes, bytes_in)

    async def test_async_socket_write_fails_when_closed(self):
        """Verifies an async socket write fails in the expected way when the other end is closed."""
        (sock_out, sock_in) = Socket.create()
        del sock_in
        with self.assertRaises(ZxStatus):
            sock_out = AsyncSocket(sock_out)
            try:
                sock_out.write(bytearray([1, 2, 3]))
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    async def test_async_socket_read_fails_when_closed(self):
        """Verifies an async socket read fails in the expected way when the other end is closed."""
        (sock_out, sock_in) = Socket.create()
        del sock_out
        with self.assertRaises(ZxStatus):
            sock_in = AsyncSocket(sock_in)
            try:
                await sock_in.read()
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e
