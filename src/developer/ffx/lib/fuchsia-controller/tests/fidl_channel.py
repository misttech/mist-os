# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from fuchsia_controller_py import FidlChannel, ZxStatus
import unittest


class FidlChannelTests(unittest.TestCase):
    """FidlChannel tests."""

    def test_channel_write_then_read(self):
        (a, b) = FidlChannel.create()
        a.write((bytearray([1, 2, 3]), []))
        buf, hdls = b.read()
        self.assertEqual(buf, bytearray([1, 2, 3]))

    def test_channel_write_fails_when_closed(self):
        (a, b) = FidlChannel.create()
        del b
        with self.assertRaises(ZxStatus):
            try:
                a.write((bytearray([1, 2, 3]), []))
            except ZxStatus as e:
                self.assertEqual(e.args[0], ZxStatus.ZX_ERR_PEER_CLOSED)
                raise e

    def test_channel_passing(self):
        (a, b) = FidlChannel.create()
        (c, d) = FidlChannel.create()
        # This is using 'take' rather than 'as_int' as using 'as_int' would cause a double-close
        # error on a channel that has already been closed.
        a.write((bytearray(), [(0, c.take(), 0, 0, 0)]))
        _, hdls = b.read()
        self.assertEqual(len(hdls), 1)
        new_c = hdls[0]
        new_c.write((bytearray([1, 2, 3]), []))
        buf, d_hdls = d.read()
        self.assertEqual(buf, bytearray([1, 2, 3]))
