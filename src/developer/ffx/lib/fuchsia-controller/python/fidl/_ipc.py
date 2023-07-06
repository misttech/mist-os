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


class _QueueWrapper(object):

    def __init__(self):
        self.queue = asyncio.Queue()
        self.loop = asyncio.get_running_loop()

    def _precheck(self):
        """Checks if this queue is being used across loops. If it is, then this will reset state."""
        if self.loop.is_closed():
            self.loop = asyncio.get_running_loop()
            self.queue = asyncio.Queue()

    def get(self):
        self._precheck()
        return self.queue.get()

    def put_nowait(self, item: int):
        self._precheck()
        self.queue.put_nowait(item)

    def task_done(self):
        self._precheck()
        self.queue.task_done()


HANDLE_READY_QUEUES: typing.Dict[int, _QueueWrapper] = {}


def enqueue_ready_zx_handle_from_fd(
        fd: int, handle_ready_queues: typing.Dict[int, _QueueWrapper]):
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
        self.handle_ready_queues = HANDLE_READY_QUEUES

    def register(self, channel: fc.FidlChannel):
        channel_number = channel.as_int()
        if channel_number not in self.handle_ready_queues:
            self.handle_ready_queues[channel_number] = _QueueWrapper()
        notification_fd = fc.connect_handle_notifier()
        # Calling this multiple times only overwrites the reader.
        # In the event that the loop is destroyed this will be removed automatically.
        asyncio.get_running_loop().add_reader(
            notification_fd,
            enqueue_ready_zx_handle_from_fd,
            notification_fd,
            self.handle_ready_queues,
        )

    def unregister(self, channel: fc.FidlChannel):
        logging.debug(f"Unregistering channel: {channel.as_int()}")
        channel_number = channel.as_int()
        if channel_number in self.handle_ready_queues:
            self.handle_ready_queues.pop(channel_number)

    def post_channel_ready(self, channel: fc.FidlChannel):
        logging.debug(f"Re-notifying for channel: {channel.as_int()}")
        self.handle_ready_queues[channel.as_int()].put_nowait(channel.as_int())

    async def wait_channel_ready(self, channel: fc.FidlChannel) -> int:
        res = await self.handle_ready_queues[channel.as_int()].get()
        self.handle_ready_queues[channel.as_int()].task_done()
        return res
