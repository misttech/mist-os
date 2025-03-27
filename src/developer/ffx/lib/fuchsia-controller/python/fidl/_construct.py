# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import inspect
import sys
import typing
from enum import Enum
from types import NoneType, UnionType
from typing import Any, Dict, ForwardRef, Optional, TypeVar, Union

from ._client import EventHandlerBase, FidlClient
from ._fidl_common import camel_case_to_snake_case
from ._server import ServerBase

T = TypeVar("T")

_ZX_TYPES = [
    "zx.handle",
    "zx.channel",
    "zx.socket",
    "zx.event",
]


def construct_response_object(response_ident: str, response_obj: Any) -> Any:
    obj = make_default_obj_from_ident(response_ident)
    return construct_result(obj, response_obj)


def construct_result(constructed_obj: T, parsed_obj: Any) -> T:
    if constructed_obj is None:
        # TODO(https://fxbug.dev/401591827): It's not entirely understood why PythonDictVisitor
        # returns the string "{}" for this case, but it always should.
        assert (
            parsed_obj == "{}"
        ), f"Failed to construct a result from {parsed_obj!r} into None."
        return None

    if getattr(constructed_obj, "__fidl_kind__", None) == "union":
        # Union types only contain one variant when decoded, so take the first key.
        key = camel_case_to_snake_case(next(iter(parsed_obj.keys())))
        try:
            sub_obj_type = getattr(constructed_obj, f"{key}_type")
        except AttributeError:
            # When using the static bindings, the type can be read directly from each variant
            # itself.
            sub_obj_type = inspect.get_annotations(
                type(constructed_obj), eval_str=True
            )[key]
        sub_parsed_obj = parsed_obj[key]
        setattr(constructed_obj, key, None)
        return construct_from_name_and_type(
            constructed_obj, sub_parsed_obj, key, sub_obj_type
        )
    elements = inspect.get_annotations(type(constructed_obj), eval_str=True)
    for name, ty in elements.items():
        sub_parsed_obj = parsed_obj.get(name)
        constructed_obj = construct_from_name_and_type(
            constructed_obj, sub_parsed_obj, name, ty
        )
    return constructed_obj


def make_default_obj_from_ident(ident: str) -> Any:
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
    try:
        mod = sys.modules[library]
    except KeyError:
        # Try using fidl_ as the prefix because static FIDL bindings may
        # be available.
        library = "fidl_" + split[0].replace(".", "_")
        mod = sys.modules[library]
    obj_ty = getattr(mod, ty)
    return obj_ty.make_default()


def unwrap_innermost_type(
    ty: Any,
    globalns: Optional[Dict[str, Any]] = None,
    localns: Optional[Dict[str, Any]] = None,
    _original_ty: Any = None,
) -> type:
    """Takes a type `ty`, then removes the meta-typing surrounding it.

    This function recursively removes meta-typing and *DOES NOT* support multiple type arguments at
    at any level of recursion. For example, calling this function on the type `tuple[int, str]` will
    raise an AssertionError.

    Args:
      ty: a Python type.

    Returns:
        The Python type after removing indirection.

    This is because FIDL libraries may include recursive types, and resolving them must be deferred
    to the moment of encoding and decoding after all libraries have been loaded.
    """
    # Keep the original type unwrap_innermost_type was called with for a better exception message.
    if _original_ty is None:
        _original_ty = ty

    # ForwardRef of a _ZX_TYPE
    if isinstance(ty, ForwardRef) and ty.__forward_arg__ in _ZX_TYPES:
        return int

    # ForwardRef of any type
    if isinstance(ty, ForwardRef):
        # TODO(https://fxbug.dev/396778959): This is a funny way to resolve a ForwardRef into
        # its inner stringized type without a pulic Python API that directly does the
        # resolution. In newer versions of Python, ForwardRef will gain an evaluate() method to
        # simplify this. (This effectively stabilizes the existing private _evaluate() method in
        # Python 3.11.)
        #
        # Suppress mypy for this line because `ty` cannot be known statically.
        def _f() -> ty:
            pass

        try:
            return unwrap_innermost_type(
                typing.get_type_hints(_f, globalns=globalns, localns=localns)[
                    "return"
                ],
                _original_ty=_original_ty,
            )
        except NameError as e:
            e.add_note(f"Failed unwrapping a ForwardRef: {_original_ty}")
            raise e

    ty_args = typing.get_args(ty)

    # Base Case. No more meta-typing exists.
    if len(ty_args) == 0:
        return ty

    # Simple layer of meta-typing with a single type arguments.
    if len(ty_args) == 1:
        return unwrap_innermost_type(ty_args[0], _original_ty=_original_ty)

    # Meta-typing layer that's effectively an Optional. The Optional type technically has two
    # arguments, but unwrap_innermost_type discards the type(None).
    if (
        len(ty_args) == 2
        and NoneType in ty_args
        and typing.get_origin(ty) in (Union, UnionType)
    ):
        ty_args_mut = list(ty_args)
        ty_args_mut.remove(type(None))
        return unwrap_innermost_type(ty_args_mut[0], _original_ty=_original_ty)

    # Meta-typing layer that allows unions of IntEnum, IntFlag, and int because IntEnum and IntFlag
    # are subclasses of int. This special case is an affordance made to support decode into static
    # FIDL binding types.
    if (
        len(ty_args) == 2
        and typing.get_origin(ty) in (Union, UnionType)
        and all(issubclass(x, int) for x in ty_args)
    ):
        assert ty_args[0].__module__.startswith(
            "fidl_"
        ), f"Encountered union of int with non-static FIDL binding type: {_original_ty}"
        ty_args_mut = list(ty_args)
        ty_args_mut.remove(int)  # Retain the static FIDL binding type.
        return unwrap_innermost_type(ty_args_mut[0], _original_ty=_original_ty)

    # The meta-typing layer is not an Optional, not an instance of ForwardRef, and has multiple
    # arguments.
    raise RuntimeError(
        f"Failed to remove meta-typing with multiple type arguments: {_original_ty}"
    )


def _is_basic_fidl_type(ty: type) -> bool:
    return ty in [bool, int, float, str]


# Assert that `value` is compatible with FIDL type `ty`. Some FIDL types are represented by int, so an
# allowance is made to decode a ty from an int if `ty` if a fidl* module.
def _assert_compatible_fidl_type(value: Any, ty: type) -> None:
    assert isinstance(value, ty) or (
        getattr(ty, "__module__", "").startswith("fidl")
        and isinstance(value, int)
    ), f"Encountered item of the wrong type: {value!r} is not a {ty}"


def construct_from_name_and_type(
    constructed_obj: T, sub_parsed_obj: Any, name: str, ty: type
) -> T:
    # Regardless of the value of ty, there is nothing to do but assign None when sub_parsed_obj is
    # None.
    if sub_parsed_obj is None:
        setattr(constructed_obj, name, None)
        return constructed_obj

    unwrapped_ty = unwrap_innermost_type(ty)

    # Check for a basic FIDL type that cannot be unwrapped.
    if _is_basic_fidl_type(type(sub_parsed_obj)):
        _assert_compatible_fidl_type(sub_parsed_obj, unwrapped_ty)
        setattr(constructed_obj, name, sub_parsed_obj)
        return constructed_obj

    # Special case for library types that are expected to be assigned directly from the parsed
    # object which must be an int.
    if (
        issubclass(unwrapped_ty, ServerBase)
        or issubclass(unwrapped_ty, FidlClient)
        or issubclass(unwrapped_ty, EventHandlerBase)
    ):
        assert isinstance(
            sub_parsed_obj, int
        ), f"Received {unwrapped_ty} not represented as an int: {sub_parsed_obj}"
        setattr(constructed_obj, name, sub_parsed_obj)
        return constructed_obj

    # The two primary recursive cases in this function are when sub_parsed_obj is either a dict or
    # list. When sub_parsed_obj is a dict, this function assumes the contents of the dict are the
    # key value pair of some FIDL type, whether it's a struct, table, etc.
    if isinstance(sub_parsed_obj, dict):
        sub_obj = unwrapped_ty.make_default()  # type: ignore[attr-defined]
        sub_obj = construct_result(sub_obj, sub_parsed_obj)
        setattr(constructed_obj, name, sub_obj)
        return constructed_obj

    # When sub_parsed_obj is a list, this function constructs a list of the corresponding type.
    # It is defined recursively in case there are nested lists.
    # The corresponding object attribute is then set and the object is returned.
    if isinstance(sub_parsed_obj, list):

        def handle_list(spo: list[Any]) -> list[Any]:
            results: list[Any] = []
            for item in spo:
                if item is None:
                    results.append(None)
                    continue

                if _is_basic_fidl_type(type(item)):
                    _assert_compatible_fidl_type(item, unwrapped_ty)
                    results.append(item)
                    continue

                if isinstance(item, list):
                    results.append(handle_list(item))
                    continue

                sub_obj = unwrapped_ty.make_default()  # type: ignore[attr-defined]
                if isinstance(sub_obj, Enum):
                    # This is a bit of a special case that can't be set from behind a function,
                    # so the variable has to be set directly. This is also the case for bits
                    # (both types are represented as enums).
                    results.append(item)
                    continue

                results.append(construct_result(sub_obj, item))

            return results

        setattr(constructed_obj, name, handle_list(sub_parsed_obj))
        return constructed_obj

    raise RuntimeError(f"Unable to construct field in object. {name}, {ty}")
