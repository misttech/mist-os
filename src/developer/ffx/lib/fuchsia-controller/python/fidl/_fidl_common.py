# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import sys
import re
import inspect
import typing
from typing import List, Tuple, Dict

# These can be updated to use TypeAlias when python is updated to 3.10+
TXID_Type = int
FidlMessage = Tuple[bytearray, List[int]]

# The number of bytes in a FIDL header.
FIDL_HEADER_SIZE = 8
# The number of bytes in a FIDL ordinal.
FIDL_ORDINAL_SIZE = 8


def parse_txid(msg: FidlMessage):
    (b, _) = msg
    return int.from_bytes(b[0:4], sys.byteorder)


def parse_ordinal(msg: FidlMessage):
    (b, _) = msg
    start = FIDL_HEADER_SIZE
    end = FIDL_HEADER_SIZE + FIDL_ORDINAL_SIZE
    return int.from_bytes(b[start:end], sys.byteorder)


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
    return make_default_obj(getattr(mod, ty))


def construct_response_object(response_ident: str, response_obj):
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
    unwrapped_module = unwrapped_ty.__module__
    if unwrapped_module.startswith("fidl."):
        obj = get_type_from_import(
            f"{unwrapped_module}.{unwrapped_ty.__name__}")
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
    obj_type = getattr(obj_type, f"{key}_type")
    union_type = unwrap_type(obj_type)
    if union_type.__module__.startswith("fidl."):
        obj = get_type_from_import(
            f"{union_type.__module__}.{union_type.__name__}")
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
