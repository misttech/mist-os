# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import typing
import fuchsia_controller_py as fc
from fidl_codec import decode_fidl_request, encode_fidl_message
from dataclasses import dataclass

from ._ipc import GlobalChannelWaker
from ._fidl_common import *


@dataclass
class MethodInfo:
    name: str
    request_ident: str
    requires_response: bool
    empty_response: bool


Ordinal = int


class ServerBase(object):
    """Base object for doing basic FIDL server tasks."""

    library: str = ""
    method_map: typing.Dict[Ordinal, MethodInfo] = {}

    class StopService(Exception):
        """StopService is used to stop the serving loop, close the channel, and shutdown cleanly."""
        pass

    def __init__(self, channel: fc.Channel, channel_waker=None):
        self.channel = channel
        if channel_waker is None:
            self.channel_waker = GlobalChannelWaker()
        else:
            self.channel_waker = channel_waker

    def __del__(self):
        if self.channel is not None:
            self.channel_waker.unregister(self.channel)

    def serve(self):
        self.channel_waker.register(self.channel)

        async def _serve():
            self.channel_waker.register(self.channel)
            while await self.handle_next_request():
                pass

        return _serve()

    async def handle_next_request(self) -> bool:
        try:
            return await self._handle_request_helper()
        except type(self).StopService:
            self.channel.close()
            return False
        except Exception as e:
            # It's very important to close the channel, because if this is run inside a task,
            # then it isn't possible for the exception to get raised in time. So if another
            # coroutine depends on this server functioning (like a client), then it'll hang
            # forever. So, we must close the channel in order to make progress.
            self.channel.close()
            self.channel = None
            raise e

    async def _handle_request_helper(self) -> bool:
        try:
            msg, txid, ordinal = await self._channel_read_and_parse()
        except fc.ZxStatus as e:
            if e.args[0] == fc.ZxStatus.ZX_ERR_PEER_CLOSED:
                return False
            else:
                raise e
        info = self.method_map[ordinal]
        ident = info.request_ident
        method_name = info.name
        method = getattr(self, method_name)
        if msg is not None:
            res = method(msg)
        else:
            res = method()
        if asyncio.iscoroutine(res) or asyncio.isfuture(res):
            res = await res
        if res is not None and not info.requires_response:
            raise RuntimeError(
                f"Method {info.name} received a response when it is a one-way method"
            )
        if res is None and info.requires_response:
            raise RuntimeError(
                f"Method {info.name} returned None when a response was expected"
            )
        if res is not None:
            fidl_msg = encode_fidl_message(
                ordinal=ordinal,
                object=res,
                library=self.library,
                txid=txid,
                type_name=res.__fidl_type__)
            self.channel.write(fidl_msg)
        elif info.empty_response:
            fidl_msg = encode_fidl_message(
                ordinal=ordinal,
                object=None,
                library=self.library,
                txid=txid,
                type_name=None)
            self.channel.write(fidl_msg)
        return True

    async def _channel_read(self) -> FidlMessage:
        try:
            msg = self.channel.read()
        except fc.ZxStatus as e:
            if e.args[0] != fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
                self.channel_waker.unregister(self.channel)
                raise e
            await self.channel_waker.wait_channel_ready(self.channel)
            msg = self.channel.read()
        return msg

    async def _channel_read_and_parse(self):
        raw_msg = await self._channel_read()
        ordinal = parse_ordinal(raw_msg)
        txid = parse_txid(raw_msg)
        handles = [x.take() for x in raw_msg[1]]
        msg = decode_fidl_request(bytes=raw_msg[0], handles=handles)
        result_obj = construct_response_object(
            self.method_map[ordinal].request_ident, msg)
        return result_obj, txid, ordinal
