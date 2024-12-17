# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# TODO(https://fxbug.dev/346628306): Remove this comment to ignore mypy errors.
# mypy: ignore-errors

import asyncio
import logging
import typing
from inspect import getframeinfo, stack

import fuchsia_controller_py as fc
from fidl_codec import decode_fidl_request, encode_fidl_message

from ._fidl_common import *
from ._ipc import GlobalHandleWaker

# Rather than make a long server UUID, this will be a monotonically increasing
# ID to differentiate servers for debugging purposes.
_SERVER_ID = 0
_LOGGER = logging.getLogger("fidl.server")


class ServerError(Exception):
    pass


class ServerBase(object):
    """Base object for doing basic FIDL server tasks."""

    library: str = ""
    method_map: typing.Dict[Ordinal, MethodInfo] = {}

    def __str__(self):
        return f"server:{self.__name__}:{self.id}"

    def __init__(self, channel: fc.Channel, channel_waker=None):
        global _SERVER_ID
        self.channel = channel
        self.id = _SERVER_ID
        _SERVER_ID += 1
        if channel_waker is None:
            self.channel_waker = GlobalHandleWaker()
        else:
            self.channel_waker = channel_waker
        caller = getframeinfo(stack()[1][0])
        _LOGGER.debug(
            f"{self} instantiated from {caller.filename}:{caller.lineno}"
        )

    def __del__(self):
        _LOGGER.debug(f"{self} closing")
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
            # TODO(b/299946378): Handle case where ordinal is unknown.
            return await self._handle_request_helper()
        except StopServer:
            self.channel.close()
            return False
        except Exception as e:
            # It's very important to close the channel, because if this is run inside a task,
            # then it isn't possible for the exception to get raised in time. So if another
            # coroutine depends on this server functioning (like a client), then it'll hang
            # forever. So, we must close the channel in order to make progress.
            self.channel.close()
            self.channel = None
            _LOGGER.debug(f"{self} request handling error: {e}")
            raise e

    async def _handle_request_helper(self) -> bool:
        # TODO(b/303532690): When attempting to decode a method that is
        # unrecognized, there should be a message sent declaring this is
        # an unknown method.
        try:
            msg, txid, ordinal = await self._channel_read_and_parse()
        except fc.ZxStatus as e:
            if e.args[0] == fc.ZxStatus.ZX_ERR_PEER_CLOSED:
                _LOGGER.debug(f"{self} shutting down. PEER_CLOSED received")
                return False
            else:
                _LOGGER.warn(f"{self} channel received error: {e}")
                raise e
        info = self.method_map[ordinal]
        info.request_ident
        method_name = info.name
        method = getattr(self, method_name)
        if msg is not None:
            res = method(msg)
        else:
            res = method()
        if asyncio.iscoroutine(res) or asyncio.isfuture(res):
            res = await res
        if res is not None and not info.requires_response:
            raise ServerError(
                f"{self} method {info.name} received a "
                + "response but is one-way method"
            )
        if res is None and info.requires_response:
            raise ServerError(
                f"{self} method {info.name} returned "
                + "None when a response was expected"
            )
        if info.has_result:
            _LOGGER.debug(f"{self} received method response {res}")
            if type(res) is DomainError:
                res = GenericResult(
                    fidl_type=info.response_identifier, err=res.error
                )
            elif type(res) is FrameworkError:
                res = GenericResult(
                    fidl_type=info.response_identifier, framework_err=res
                )
            else:
                res = GenericResult(
                    fidl_type=info.response_identifier, response=res
                )
        if res is not None:
            encoded_fidl_message = encode_fidl_message(
                ordinal=ordinal,
                object=res,
                library=self.library,
                txid=txid,
                type_name=res.__fidl_raw_type__,
            )
            self.channel.write(encoded_fidl_message)
        elif info.empty_response:
            encoded_fidl_message = encode_fidl_message(
                ordinal=ordinal,
                object=None,
                library=self.library,
                txid=txid,
                type_name=None,
            )
            self.channel.write(encoded_fidl_message)
        return True

    async def _channel_read(self) -> FidlMessage:
        while True:
            try:
                return self.channel.read()
            except fc.ZxStatus as e:
                # Any number of spurious wakeups are possible. Stay in the loop if the error
                # is ZX_ERR_SHOULD_WAIT.
                if e.args[0] == fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
                    _LOGGER.debug(f"{self} channel spurious wakeup")
                    await self.channel_waker.wait_channel_ready(self.channel)
                    continue
                self.channel_waker.unregister(self.channel)
                _LOGGER.warning(f"{self} channel received error: {e}")
                raise e

    async def _channel_read_and_parse(self):
        raw_msg = await self._channel_read()
        ordinal = parse_ordinal(raw_msg)
        txid = parse_txid(raw_msg)
        handles = [x.take() for x in raw_msg[1]]
        msg = decode_fidl_request(bytes=raw_msg[0], handles=handles)
        result_obj = construct_response_object(
            self.method_map[ordinal].request_ident, msg
        )
        return result_obj, txid, ordinal

    def _send_event(self, ordinal: int, library: str, msg_obj):
        type_name = None
        if msg_obj is not None:
            type_name = msg_obj.__fidl_raw_type__
        encoded_fidl_message = encode_fidl_message(
            ordinal=ordinal,
            object=msg_obj,
            library=library,
            txid=0,
            type_name=type_name,
        )
        self.channel.write(encoded_fidl_message)
