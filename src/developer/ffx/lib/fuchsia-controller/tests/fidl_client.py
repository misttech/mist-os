# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import unittest
import fidl.fuchsia_developer_ffx as ffx_fidl
import os
import tempfile
import os.path
import asyncio
import fidl.fuchsia_developer_ffx as ffx
from fuchsia_controller_py import Context, IsolateDir, FidlChannel, ZxStatus
from fidl_codec import encode_fidl_message, method_ordinal
from unittest.mock import Mock, patch


class FidlClientTests(unittest.IsolatedAsyncioTestCase):
    """Fidl Client tests."""

    async def test_read_and_decode_staged_message(self):
        channel = Mock()
        channel.__class__ = FidlChannel
        channel.as_int.return_value = 0
        channel.read.side_effect = [
            (bytearray([2, 0, 0, 0]), []),
            ZxStatus(ZxStatus.ZX_ERR_SHOULD_WAIT),
            (bytearray([1, 0, 0, 0]), []),
        ]
        # The proxy here really doesn't matter, we're trying to access internal methods.
        proxy = ffx.Echo.Client(channel)
        proxy.pending_txids.add(1)
        proxy.pending_txids.add(2)
        proxy.channel_waker.loop_readers = {}
        proxy.channel_waker.handle_ready_queues = {}
        proxy.channel_waker.handle_ready_queues[0] = asyncio.Queue()
        proxy.channel_waker.handle_ready_queues[0].put_nowait(0)
        proxy._decode = Mock()
        proxy._decode.return_value = (bytearray([1, 2, 3]), [])
        loop = asyncio.get_running_loop()
        first_task = loop.create_task(proxy._read_and_decode(1))
        second_task = loop.create_task(proxy._read_and_decode(2))
        await asyncio.gather(first_task, second_task)

    async def test_read_and_decode_blocked(self):
        channel = Mock()
        channel.__class__ = FidlChannel
        channel.as_int.return_value = 0
        channel.read.side_effect = [
            ZxStatus(ZxStatus.ZX_ERR_SHOULD_WAIT),
            ZxStatus(ZxStatus.ZX_ERR_SHOULD_WAIT),
            ZxStatus(ZxStatus.ZX_ERR_SHOULD_WAIT),
            (bytearray([1, 0, 0, 0]), []),
        ]
        proxy = ffx.Echo.Client(channel)
        proxy.pending_txids.add(1)
        proxy.channel_waker.loop_readers = {}
        proxy.channel_waker.handle_ready_queues = {}
        proxy.channel_waker.handle_ready_queues[0] = asyncio.Queue()
        proxy.channel_waker.handle_ready_queues[0].put_nowait(0)
        proxy.channel_waker.handle_ready_queues[0].put_nowait(0)
        proxy.channel_waker.handle_ready_queues[0].put_nowait(0)
        proxy._decode = Mock()
        proxy._decode.return_value = (bytearray([1, 2, 3]), [])
        await proxy._read_and_decode(1)

    async def test_read_and_decode_simul_notification(self):
        channel = Mock()
        channel.__class__ = FidlChannel
        channel.as_int.return_value = 0
        channel.read.side_effect = [
            ZxStatus(ZxStatus.ZX_ERR_SHOULD_WAIT),
            ZxStatus(ZxStatus.ZX_ERR_SHOULD_WAIT),
        ]
        proxy = ffx.Echo.Client(channel)
        proxy.pending_txids.add(1)
        proxy.channel_waker.loop_readers = {}
        proxy.channel_waker.handle_ready_queues = {}
        proxy.channel_waker.handle_ready_queues[0] = asyncio.Queue()
        proxy.channel_waker.handle_ready_queues[0].put_nowait(0)
        proxy.staged_messages[1] = asyncio.Queue(1)
        proxy.staged_messages[1].put_nowait((bytearray([1, 0, 0, 0]), []))
        proxy._decode = Mock()
        proxy._decode.return_value = (bytearray([1, 2, 3]), [])
        await proxy._read_and_decode(1)

    async def test_unexpected_txid(self):
        channel = Mock()
        channel.__class__ = FidlChannel
        channel.as_int.return_value = 0
        channel.read.side_effect = [(bytearray([1, 0, 0, 0]), ())]
        proxy = ffx.Echo.Client(channel)
        proxy.channel_waker.loop_readers = {}
        proxy.channel_waker.handle_ready_queues = {}
        proxy.channel_waker.handle_ready_queues[0] = asyncio.Queue()
        proxy.channel_waker.handle_ready_queues[0].put_nowait(0)
        with self.assertRaises(RuntimeError):
            await proxy._read_and_decode(10)
        self.assertEqual(proxy.channel, None)

    async def test_staging_stages(self):
        channel = Mock()
        channel.__class__ = FidlChannel
        channel.as_int.return_value = 0
        proxy = ffx.Echo.Client(channel)
        proxy.pending_txids.add(1)
        expect = (bytearray([1, 2, 3]), [])
        proxy._stage_message(1, expect)
        self.assertEqual(len(proxy.staged_messages), 1)
        got = await proxy._get_staged_message(1)
        self.assertEqual(got, expect)

        # This part is a little silly. The decode_fidl_message
        # function can't be mocked, so we're decoding with an actual
        # FIDL message we know is loaded (the Echo protocol from ffx).
        class DecodeObj:
            pass

        obj = DecodeObj()
        obj.__dict__["value"] = "foo"
        proxy._decode(
            1,
            encode_fidl_message(
                object=obj,
                library="fuchsia.developer.ffx",
                type_name="fuchsia.developer.ffx/EchoEchoStringRequest",
                txid=1,
                ordinal=method_ordinal(
                    protocol="fuchsia.developer.ffx/Echo",
                    method="EchoString")))
        # Verifies state is cleaned up.
        self.assertEqual(len(proxy.staged_messages), 0)
        self.assertEqual(len(proxy.pending_txids), 0)
