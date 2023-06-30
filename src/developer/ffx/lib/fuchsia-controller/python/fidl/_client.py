# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import sys
import inspect
import logging
import fuchsia_controller_py as fc
from fidl_codec import decode_fidl_response
from fidl_codec import encode_fidl_message
import typing
from typing import Dict, List, Tuple, Set

from ._ipc import GlobalChannelWaker

# These can be updated to use TypeAlias when python is updated to 3.10+
TXID_Type = int
FidlMessage = Tuple[bytearray, List[int]]

TXID: TXID_Type = 0


def get_type_from_import(i):
    """Takes an import and returns the Python type.

    Args:
        i: The python FIDL import string, e.g. "fidl.foo_bar_baz.Mumble" and returns the type class
        Mumble.

    Returns:
        The Python type class of the object.
    """
    module_path = i.split(".")
    fidl_import_path = f"{module_path[0]}.{module_path[1]}"
    mod = sys.modules[fidl_import_path]
    obj = mod
    for attr in module_path[2:]:
        obj = getattr(obj, attr)
    return obj


def make_default_obj_from_ident(ident):
    """Takes a FIDL identifier, e.g. foo.bar/Baz, returns the default object (all fields None).

    Args:
        ident: The FIDL identifier.

    Returns:
        The default object construction (all fields None).
    """
    # If there is not identifier then this is for a two way method that returns ().
    if not ident:
        return None
    split = ident.split("/")
    library = "fidl." + split[0].replace(".", "_")
    ty = split[1]
    mod = sys.modules[library]
    return make_default_obj(getattr(mod, ty))


def unwrap_type(ty):
    """Takes a type `ty`, then removes the meta-typing surrounding it.

    Args:
      ty: a Python type.

    Returns:
        The Python type after removing indirection.

    This is because when a user imports fidl.[foo_library], they may import a recursive type, which
    cannot be defined at runtime. This will then return an actual type (since everything will be
    resolvable at this point).
    """
    while True:
        try:
            ty = typing.get_args(ty)[0]
        except IndexError:
            if ty.__class__ is typing.ForwardRef:
                return ty.__forward_arg__
            return ty


def construct_from_name_and_type(constructed_obj, sub_parsed_obj, name, ty):
    unwrapped_ty = unwrap_type(ty)
    if str(unwrapped_ty).startswith("fidl."):
        obj = get_type_from_import(str(unwrapped_ty))
        is_union = False
        try:
            is_union = obj.__fidl_kind__ == "union"
        except AttributeError:
            pass

        def handle_union(parsed_obj):
            if not is_union:
                sub_obj = make_default_obj(obj)
                construct_result(sub_obj, parsed_obj)
            else:
                sub_obj = construct_from_union(obj, parsed_obj)
            return sub_obj

        if isinstance(sub_parsed_obj, dict):
            setattr(constructed_obj, name, handle_union(sub_parsed_obj))
        elif isinstance(sub_parsed_obj, list):
            results = []
            for item in sub_parsed_obj:
                results.append(handle_union(item))
            setattr(constructed_obj, name, results)
        else:
            setattr(constructed_obj, name, sub_parsed_obj)
    else:
        setattr(constructed_obj, name, sub_parsed_obj)


def construct_from_union(obj_type, parsed_obj):
    assert obj_type.__fidl_kind__ == "union"
    assert len(parsed_obj.keys()) == 1
    key = next(iter(parsed_obj.keys()))
    sub_parsed_obj = parsed_obj[key]
    obj_type = getattr(obj_type, key)
    union_type = unwrap_type(obj_type)
    if str(union_type).startswith("fidl."):
        obj = get_type_from_import(str(union_type))
        sub_obj = make_default_obj(obj)
    else:
        sub_obj = make_default_obj(union_type)
    construct_result(sub_obj, sub_parsed_obj)
    return sub_obj


def construct_result(constructed_obj, parsed_obj):
    try:
        elements = type(constructed_obj).__annotations__
    except AttributeError as exc:
        if constructed_obj.__fidl_kind__ == "union":
            construct_from_union(constructed_obj, parsed_obj)
            return
        else:
            raise TypeError(
                f"Unexpected FIDL kind: {constructed_obj.__fidl_kind__}"
            ) from exc
    for name, ty in elements.items():
        if parsed_obj.get(name) is None:
            setattr(constructed_obj, name, None)
            continue
        sub_parsed_obj = parsed_obj[name]
        construct_from_name_and_type(constructed_obj, sub_parsed_obj, name, ty)


def make_default_obj(object_ty):
    """Takes a type `object_ty` and creates the default __init__ implementation of the object.

    Args:
        object_ty: The type of object which is being constructed (this is also the type of the
        return value).

    Returns:
        The default (all fields None) object created from object_ty.

    For example, if the object is a struct, it will return the "default" version of the struct,
    where all fields are set to None, regardless what the field type is.
    """
    sig = inspect.signature(object_ty.__init__)
    args = {}
    for arg in sig.parameters:
        if str(arg) == "self":
            continue
        args[str(arg)] = None
    if not args:
        return object_ty()
    try:
        return object_ty(**args)
    except TypeError:
        # Object might accept *args/**kwargs, so use empty constructor.
        return object_ty()


def parse_txid(msg: FidlMessage):
    (b, _) = msg
    return int.from_bytes(b[0:4], sys.byteorder)


class FidlClient(object):

    def __init__(self, channel, channel_waker=None):
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
                    self.channel = None
                    raise RuntimeError(
                        "Received unexpected TXID. Channel closed and invalid. "
                        +
                        "Continuing to use this FIDL client after this exception will result "
                        + "in undefined behavior")
                self._stage_message(recvd_txid, msg)
            except fc.ZxStatus as e:
                if e.args[0] != fc.ZxStatus.ZX_ERR_SHOULD_WAIT:
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

    async def _send_two_way_fidl_request(
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
        res = await self._read_and_decode(TXID)
        result_obj = make_default_obj_from_ident(response_ident)
        if result_obj is not None:
            construct_result(result_obj, res)
        return result_obj

    def _send_one_way_fidl_request(
            self, txid: int, ordinal: int, library: str, msg_obj):
        """Sends a synchronous one-way FIDL request.

        Args:
            ordinal: The method ordinal (for encoding).
            library: The FIDL library from which this method ordinal exists.
            msg_obj: The object being sent.
        """
        type_name = None
        if msg_obj is not None and type_name is None:
            type_name = msg_obj.__fidl_type__
        fidl_message = encode_fidl_message(
            ordinal=ordinal,
            object=msg_obj,
            library=library,
            txid=txid,
            type_name=type_name)
        self.channel.write(fidl_message)
