# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Module for handling encoding and decoding FIDL messages, as well as for handling async I/O."""
import asyncio
import logging
import os
import sys
import typing
import fuchsia_controller_py as fc

HANDLE_READY_QUEUES: typing.Dict[int, asyncio.Queue] = {}
LOOP_READERS: typing.Dict[asyncio.AbstractEventLoop, typing.Set[int]] = {}


def enqueue_ready_zx_handle_from_fd(
        fd: int, handle_ready_queues: typing.Dict[int, asyncio.Queue]):
    """Reads zx_handle that is ready for reading, and enqueues it in the appropriate ready queue."""
    handle_no = int.from_bytes(os.read(fd, 4), sys.byteorder)
    queue = handle_ready_queues.get(handle_no)
    if queue:
        queue.put_nowait(handle_no)
    else:
        logging.debug(f"Dropping notification for: {handle_no}")


class GlobalChannelWaker(object):
    """A class for handling notifications on a channel. By default hooks into global state."""

    def __init__(self):
        self.loop_readers = LOOP_READERS
        self.handle_ready_queues = HANDLE_READY_QUEUES

    def register(self, channel: fc.FidlChannel):
        loop = asyncio.get_running_loop()
        channel_number = channel.as_int()
        if channel_number not in self.handle_ready_queues:
            ready_queue: asyncio.Queue[int] = asyncio.Queue()
            self.handle_ready_queues[channel_number] = ready_queue
        if loop not in self.loop_readers:
            notification_fd = fc.connect_handle_notifier()
            loop.add_reader(
                notification_fd,
                enqueue_ready_zx_handle_from_fd,
                notification_fd,
                self.handle_ready_queues,
            )
            self.loop_readers[loop] = set()
        self.loop_readers[loop].add(channel.as_int())

    def unregister(self, channel: fc.FidlChannel):
        logging.debug(f"Unregistering channel: {channel.as_int()}")
        channel_number = channel.as_int()
        if channel_number in self.handle_ready_queues:
            self.handle_ready_queues.pop(channel_number)
        # This may be called from inside an exception, so it's possible there is no running loop
        # available.
        try:
            loop = asyncio.get_running_loop()
            if loop in self.loop_readers:
                reader_set = self.loop_readers[loop]
                if channel_number in reader_set:
                    reader_set.remove(channel_number)
                if not reader_set:
                    self.loop_readers.pop(loop)
                    loop.remove_reader(fc.connect_handle_notifier())
        except RuntimeError:
            pass

    def post_channel_ready(self, channel: fc.FidlChannel):
        logging.debug(f"Re-notifying for channel: {channel.as_int()}")
        self.handle_ready_queues[channel.as_int()].put_nowait(channel.as_int())

    async def wait_channel_ready(self, channel: fc.FidlChannel) -> int:
        res = await self.handle_ready_queues[channel.as_int()].get()
        self.handle_ready_queues[channel.as_int()].task_done()
        return res
