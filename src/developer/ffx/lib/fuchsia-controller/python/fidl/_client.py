# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import logging
import fuchsia_controller_py as fc
from fidl_codec import decode_fidl_response
from fidl_codec import encode_fidl_message
import typing
from typing import Dict, Set

from ._ipc import GlobalChannelWaker
from ._fidl_common import *

TXID: TXID_Type = 0


class FidlClient(object):

    def __init__(self, channel, channel_waker=None):
        if type(channel) == int:
            self.channel = fc.Channel(channel)
        else:
            self.channel = channel
        if channel_waker is None:
            self.channel_waker = GlobalChannelWaker()
        else:
            self.channel_waker = channel_waker
        self.pending_txids: Set[TXID_Type] = set({})
        self.staged_messages: Dict[TXID_Type, asyncio.Queue[FidlMessage]] = {}

    def __del__(self):
        if self.channel is not None:
            self.channel_waker.unregister(self.channel)

    async def _get_staged_message(self, txid: TXID_Type):
        res = await self.staged_messages[txid].get()
        self.staged_messages[txid].task_done()
        return res

    def _stage_message(self, txid: TXID_Type, msg: FidlMessage):
        # This should only ever happen if we're a channel reading another channel's response before
        # it has ever made a request.
        if txid not in self.staged_messages:
            self.staged_messages[txid] = asyncio.Queue(1)
        self.staged_messages[txid].put_nowait(msg)

    def _decode(self, txid: TXID_Type, msg: FidlMessage):
        self.staged_messages.pop(txid)  # Just a memory leak prevention.
        self.pending_txids.remove(txid)
        return decode_fidl_response(bytes=msg[0], handles=msg[1])

    async def _read_and_decode(self, txid: int):
        if txid not in self.staged_messages:
            self.staged_messages[txid] = asyncio.Queue(1)
        while True:
            # The main gist of this loop is:
            # 1.) Try to read from the channel.
            #   a.) If we read the message and it matches out TXID we're done.
            #   b.) If we read the message and it doesn't match our TXID, we "stage" the message for
            #       another task to read later, then we wait.
            #   c.) If we get a ZX_ERR_SHOULD_WAIT, we need to wait.
            # 2.) Once we're waiting, we select on either the handle being ready to read again, or
            #     on a staged message becoming available.
            # 3.) If the select returns something that isn't a staged message, continue the loop
            #     again.
            #
            # It's pretty straightforward on paper but requires a bit of bookkeeping for the corner
            # cases to prevent memory leaks.
            try:
                msg = self.channel.read()
                recvd_txid = parse_txid(msg)
                if recvd_txid == txid:
                    return self._decode(txid, msg)
                if recvd_txid == 0:  # This is an event.
                    logging.warning(
                        f"FIDL channel event on chan: {self.channel.as_int()}. Currently ignoring."
                    )
                    pass
                elif recvd_txid not in self.pending_txids:
                    self.channel_waker.unregister(self.channel)
                    self.channel = None
                    raise RuntimeError(
                        "Received unexpected TXID. Channel closed and invalid. "
                        +
                        "Continuing to use this FIDL client after this exception will result "
                        + "in undefined behavior")
                self._stage_message(recvd_txid, msg)
            except fc.ZxStatus as e:
                if e.args[0] != fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
                    self.channel_waker.unregister(self.channel)
                    raise e
            loop = asyncio.get_running_loop()
            channel_waker_task = loop.create_task(
                self.channel_waker.wait_channel_ready(self.channel))
            staged_msg_task = loop.create_task(self._get_staged_message(txid))
            done, pending = await asyncio.wait(
                [
                    channel_waker_task,
                    staged_msg_task,
                ],
                return_when=asyncio.FIRST_COMPLETED)
            # Both notifications happened at the same time.
            if len(done) == 2:
                # Order of asyncio.wait is not guaranteed.
                first = done.pop().result()
                second = done.pop().result()
                if type(first) == int:
                    msg = second
                else:
                    msg = first

                # Since both the channel and the staged message were available, we've chosen to take
                # the staged message. To ensure another task can be awoken, we must post an event
                # saying the channel still needs to be read, since we've essentilly stolen it from
                # another task.
                self.channel_waker.post_channel_ready(self.channel)
                return self._decode(txid, msg)

            # Only one notification came in.
            msg = done.pop().result()
            pending.pop().cancel()
            if type(msg) != int:  # Not a FIDL channel response
                return self._decode(txid, msg)

    def _send_two_way_fidl_request(
            self, ordinal, library, msg_obj, response_ident):
        """Sends a two-way asynchronous FIDL request.

        Args:
            ordinal: The method ordinal (for encoding).
            library: The FIDL library from which this method ordinal exists.
            msg_obj: The object being sent.
            response_ident: The full FIDL identifier of the response object, e.g. foo.bar/Baz

        Returns:
            The object from the two-way function, as constructed from the response_ident type.
        """
        global TXID
        TXID += 1
        self.channel_waker.register(self.channel)
        self.pending_txids.add(TXID)
        self._send_one_way_fidl_request(TXID, ordinal, library, msg_obj)

        async def result(txid):
            # This is called a second time because the first attempt may have been in a sync context
            # and would not have added a reader.
            self.channel_waker.register(self.channel)
            res = await self._read_and_decode(txid)
            return construct_response_object(response_ident, res)

        return result(TXID)

    def _send_one_way_fidl_request(
            self, txid: int, ordinal: int, library: str, msg_obj):
        """Sends a synchronous one-way FIDL request.

        Args:
            ordinal: The method ordinal (for encoding).
            library: The FIDL library from which this method ordinal exists.
            msg_obj: The object being sent.
        """
        type_name = None
        if msg_obj is not None:
            type_name = msg_obj.__fidl_type__
        fidl_message = encode_fidl_message(
            ordinal=ordinal,
            object=msg_obj,
            library=library,
            txid=txid,
            type_name=type_name)
        self.channel.write(fidl_message)
