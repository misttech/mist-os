# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from fuchsia_controller_py import Channel, Socket, ZxStatus
import unittest


class SocketTests(unittest.TestCase):
    """Socket tests."""

    def test_socket_write_then_read(self):
        (a, b) = Socket.create()
        bytes_in = bytearray([1, 2, 3])
        a.write(bytes_in)
        bytes_out = b.read()
        self.assertEqual(bytes_out, bytes_in)

    def test_socket_write_fails_when_closed(self):
        (a, b) = Socket.create()
        del b
        with self.assertRaises(ZxStatus):
            try:
                a.write(bytearray([1, 2, 3]))
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    def test_socket_passing(self):
        (a, b) = Channel.create()
        (c, d) = Socket.create()
        # This is using 'take' rather than 'as_int' as using 'as_int' would cause a double-close
        # error on a channel that has already been closed.
        a.write((bytearray(), [(0, c.take(), 0, 0, 0)]))
        _, hdls = b.read()
        self.assertEqual(len(hdls), 1)
        bytes_in = bytearray([1, 2, 3])
        new_c = Socket(hdls[0])
        new_c.write(bytes_in)
        bytes_out = d.read()
        self.assertEqual(bytes_out, bytes_in)
