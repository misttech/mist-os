# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# TODO(https://fxbug.dev/346628306): Remove this comment to ignore mypy errors.
# mypy: ignore-errors

import enum
import inspect
import re
import sys
import typing
from dataclasses import dataclass
from enum import IntEnum
from typing import List, Tuple

import fuchsia_controller_py as fc

# These can be updated to use TypeAlias when python is updated to 3.10+
TXID_Type = int
Ordinal = int

FidlMessage = Tuple[bytearray, List[fc.Channel]]

# The number of bytes in a FIDL header.
FIDL_HEADER_SIZE = 8
# The number of bytes in a FIDL ordinal.
FIDL_ORDINAL_SIZE = 8
FIDL_EPITAPH_ORDINAL = 0xFFFFFFFFFFFFFFFF


@dataclass
class MethodInfo:
    name: str
    request_ident: str
    requires_response: bool
    empty_response: bool
    has_result: bool
    response_identifier: str


class StopServer(Exception):
    """An exception used to stop a server loop from continuing.

    This closes the underlying channel as well.
    """


@dataclass
class DomainError:
    """A class used to wrap returning an error from a two-way method."""

    error: typing.Any


class StopEventHandler(Exception):
    """An exception used to stop an event handler from continuing."""


def internal_kind_to_type(internal_kind: str):
    # TODO(https://fxbug.dev/42061151): Remove "transport_error".
    if internal_kind == "framework_error" or internal_kind == "transport_error":
        return FrameworkError
    raise RuntimeError(f"Unrecognized internal type: {internal_kind}")


class EpitaphError(fc.ZxStatus):
    """An exception received when an epitaph has been sent on the channel."""


class FrameworkError(IntEnum):
    UNKNOWN_METHOD = -2


@dataclass
class GenericResult:
    fidl_type: str
    response: typing.Optional[object] = None
    err: typing.Optional[object] = None
    framework_err: FrameworkError | None = None

    @property
    def __fidl_type__(self):
        return self.fidl_type

    @property
    def __fidl_raw_type__(self):
        return self.fidl_type


def parse_txid(msg: FidlMessage):
    (b, _) = msg
    return int.from_bytes(b[0:4], sys.byteorder)


def parse_ordinal(msg: FidlMessage):
    (b, _) = msg
    start = FIDL_HEADER_SIZE
    end = FIDL_HEADER_SIZE + FIDL_ORDINAL_SIZE
    return int.from_bytes(b[start:end], sys.byteorder)


def parse_epitaph_value(msg: FidlMessage):
    (b, _) = msg
    start = FIDL_HEADER_SIZE + FIDL_ORDINAL_SIZE
    end = start + 4
    res = int.from_bytes(b[start:end], sys.byteorder)

    # Do two's complement here to get the true value if it's negative.
    if res & (1 << 31) != 0:
        return res - (1 << 32)
    return res


def camel_case_to_snake_case(s: str):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", s).lower()


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
    obj_ty = getattr(mod, ty)
    return make_default_obj(obj_ty)


def construct_response_object(
    response_ident: str | None, response_obj: typing.Any | None
):
    obj = make_default_obj_from_ident(response_ident)
    if obj is not None:
        construct_result(obj, response_obj)
    return obj


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


def construct_from_name_and_type(constructed_obj, sub_parsed_obj, name, ty):
    unwrapped_ty = unwrap_type(ty)
    handle_types = ["zx.handle", "zx.channel", "zx.socket", "zx.event"]
    ty_str = str(unwrapped_ty)
    if ty_str in handle_types or ty_str.endswith(".Server"):
        setattr(constructed_obj, name, sub_parsed_obj)
        return
    if sub_parsed_obj is None:
        setattr(constructed_obj, name, None)
        return
    unwrapped_module = unwrapped_ty.__module__
    if unwrapped_module.startswith("fidl."):
        obj = get_type_from_import(
            f"{unwrapped_module}.{unwrapped_ty.__name__}"
        )
        if isinstance(sub_parsed_obj, dict):
            sub_obj = make_default_obj(obj)
            construct_result(sub_obj, sub_parsed_obj)
            setattr(constructed_obj, name, sub_obj)
        elif isinstance(sub_parsed_obj, list):
            # It may be necessary here to handle a list of lists.
            def handle_list(spo):
                results = []
                for item in spo:
                    if isinstance(item, list):
                        results.append(handle_list(item))
                        continue
                    sub_obj = make_default_obj(obj)
                    if item is None:
                        sub_obj = None
                    elif isinstance(sub_obj, enum.Enum):
                        # This is a bit of a special case that can't be set from behind a function,
                        # so the variable has to be set directly. This is also the case for bits
                        # (both types are represented as enums).
                        sub_obj = item
                    else:
                        construct_result(sub_obj, item)
                    results.append(sub_obj)
                return results

            setattr(constructed_obj, name, handle_list(sub_parsed_obj))
        else:
            setattr(constructed_obj, name, sub_parsed_obj)
    else:
        setattr(constructed_obj, name, sub_parsed_obj)


def construct_result(constructed_obj, parsed_obj):
    if constructed_obj.__fidl_kind__ == "union":
        key = camel_case_to_snake_case(next(iter(parsed_obj.keys())))
        sub_obj_type = getattr(constructed_obj, f"{key}_type")
        sub_parsed_obj = parsed_obj[key]
        setattr(constructed_obj, key, None)
        construct_from_name_and_type(
            constructed_obj, sub_parsed_obj, key, sub_obj_type
        )
        # Since there is only one item for the union, no need to continue with iterating
        # elements.
        return
    elements = type(constructed_obj).__annotations__
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
    args: typing.Dict[str, typing.Any | None] = {}
    for arg in sig.parameters:
        if str(arg) == "self":
            continue
        args[str(arg)] = None
    if not args:
        return object_ty()
    try:
        return object_ty(**args)
    except TypeError:
        if issubclass(object_ty, enum.Enum):
            # If this is an Enum, we will just set 0, since it must be set to
            # a specific numeric value.
            return object_ty(value=0)
        return object_ty()
