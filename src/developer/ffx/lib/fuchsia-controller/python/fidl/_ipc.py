# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Module for handling encoding and decoding FIDL messages, as well as for handling async I/O."""

import asyncio
import logging
import socket
import typing
from abc import ABC, abstractmethod

import fuchsia_controller_py as fc


class _QueueWrapper(object):
    def __init__(self) -> None:
        self.queue: asyncio.Queue[int] = asyncio.Queue()
        try:
            self.loop: asyncio.AbstractEventLoop | None = (
                asyncio.get_running_loop()
            )
        except RuntimeError:
            self.loop = None

    def _precheck(self) -> None:
        """Checks if this queue is being used across loops. If it is, then this will reset state."""
        if self.loop is None or self.loop.is_closed():
            self.loop = asyncio.get_running_loop()
            self.queue = asyncio.Queue()

    async def get(self) -> int:
        self._precheck()
        return await self.queue.get()

    def put_nowait(self, item: int) -> None:
        self._precheck()
        self.queue.put_nowait(item)

    def task_done(self) -> None:
        self._precheck()
        self.queue.task_done()


class EventWrapper(object):
    def __init__(self) -> None:
        self.event = asyncio.Event()
        try:
            self.loop: asyncio.AbstractEventLoop | None = (
                asyncio.get_running_loop()
            )
        except RuntimeError:
            self.loop = None

    def _precheck(self) -> None:
        """Checks if this event is being used across loops. If it is, then this will reset state."""
        if self.loop is None or self.loop.is_closed():
            self.loop = asyncio.get_running_loop()
            event_state: bool = self.event.is_set()
            self.event = asyncio.Event()
            if event_state:
                self.event.set()

    async def wait(self) -> bool:
        self._precheck()
        return await self.event.wait()

    def set(self) -> None:
        self._precheck()
        self.event.set()

    def is_set(self) -> None:
        self._precheck()
        self.event.is_set()


HANDLE_READY_QUEUES: typing.Dict[int, _QueueWrapper] = {}


def enqueue_ready_zx_handle_from_fd(
    fd: int, handle_ready_queues: typing.Dict[int, _QueueWrapper]
) -> None:
    """Reads zx_handle that is ready for reading, and enqueues it in the appropriate ready queue."""
    s = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)
    handle_no = int.from_bytes(s.recv(4), "little")
    queue = handle_ready_queues.get(handle_no)
    if queue:
        queue.put_nowait(handle_no)
    else:
        logging.debug(f"Dropping notification for: {handle_no}")


class HandleWaker(ABC):
    """Base class for a waker used with potentially blocking handles."""

    @abstractmethod
    def register(self, channel: fc.BaseHandle) -> None:
        """Registers a handle to receive wake notifications."""

    @abstractmethod
    def unregister(self, channel: fc.BaseHandle) -> None:
        """Unregisters a handle, meaning it is not possible to wait for it to be ready."""

    @abstractmethod
    def post_ready(self, channel: fc.BaseHandle) -> None:
        """Notifies the waker that a channel is ready."""

    @abstractmethod
    async def wait_ready(self, channel: fc.BaseHandle) -> int:
        """Waits for a channel to be ready asynchronously."""


class GlobalHandleWaker(HandleWaker):
    """A class for handling notifications on a readable handle.

    As this is a global channel waker, this hooks into global state. This is the default waker used
    with all client and server code, as well as async wrapper code around readable handles.
    """

    def __init__(self) -> None:
        self.handle_ready_queues = HANDLE_READY_QUEUES

    def register(self, h: fc.BaseHandle) -> None:
        if h.as_int() not in self.handle_ready_queues:
            self.handle_ready_queues[h.as_int()] = _QueueWrapper()
        notification_fd = fc.connect_handle_notifier()
        # This try call is simply here in the event that this registration has happened in a
        # synchronous context (which only happens now when _send_two_way_fidl_request is called
        # inside asyncio.run(...)).
        try:
            # Calling this multiple times only overwrites the reader.
            # In the event that the loop is destroyed this will be removed automatically.
            asyncio.get_running_loop().add_reader(
                notification_fd,
                enqueue_ready_zx_handle_from_fd,
                notification_fd,
                self.handle_ready_queues,
            )
        except RuntimeError:
            pass

    def unregister(self, h: fc.BaseHandle) -> None:
        if h.as_int() in self.handle_ready_queues:
            self.handle_ready_queues.pop(h.as_int())

    def post_ready(self, h: fc.BaseHandle) -> None:
        logging.debug(f"Re-notifying for channel: {h.as_int()}")
        self.handle_ready_queues[h.as_int()].put_nowait(h.as_int())

    async def wait_ready(self, h: fc.BaseHandle) -> int:
        res = await self.handle_ready_queues[h.as_int()].get()
        self.handle_ready_queues[h.as_int()].task_done()
        return res
