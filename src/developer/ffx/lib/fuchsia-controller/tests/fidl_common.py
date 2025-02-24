# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import sys
import types
import typing
import unittest
from typing import ForwardRef, Optional, Union

import fidl.fuchsia_developer_ffx as ffx
import fidl.fuchsia_net as fnet
from fidl._fidl_common import (
    camel_case_to_snake_case,
    construct_response_object,
    get_type_from_import,
    make_default_obj_from_ident,
    unwrap_innermost_type,
)


class FidlCommon(unittest.TestCase):
    """Tests for FIDL common utils"""

    def test_camel_case_to_snake_case_standard(self) -> None:
        expected = "test_string"
        got = camel_case_to_snake_case("TestString")
        self.assertEqual(expected, got)

    def test_camel_case_to_snake_case_empty_string(self) -> None:
        expected = ""
        got = camel_case_to_snake_case("")
        self.assertEqual(expected, got)

    def test_camel_case_to_snake_case_all_caps(self) -> None:
        expected = "f_f_f_f"
        got = camel_case_to_snake_case("FFFF")
        self.assertEqual(expected, got)

    def test_unwrap_multiple_layers(self) -> None:
        ty = typing.Optional[typing.Sequence[str]]
        expected = str
        got = unwrap_innermost_type(ty)
        self.assertEqual(expected, got)

    def test_unwrap_simple_forward_ref(self) -> None:
        ty = ForwardRef("str")
        expected = str
        got = unwrap_innermost_type(ty)
        self.assertEqual(expected, got)

    def test_unwrap_optional(self) -> None:
        ty = Optional[str]
        self.assertEqual(str, unwrap_innermost_type(ty))

    def test_unwrap_union_equivalent_optional(self) -> None:
        ty = Union[int, None]
        self.assertEqual(int, unwrap_innermost_type(ty))

    def test_unwrap_union_type_equivalent_optional(self) -> None:
        ty = float | None
        self.assertEqual(float, unwrap_innermost_type(ty))

    def test_unwrap_bad_union(self) -> None:
        with self.assertRaises(RuntimeError):
            unwrap_innermost_type(float | int | None)

    def test_unwrap_fidl_forward_ref(self) -> None:
        ty = ForwardRef("IpAddress", module=fnet.__name__)
        self.assertEqual(fnet.IpAddress, unwrap_innermost_type(ty))

    def test_unwrap_fidl_forward_ref_from_globals(self) -> None:
        ty = ForwardRef("fnet.IpAddress")
        self.assertEqual(
            fnet.IpAddress, unwrap_innermost_type(ty, globalns=globals())
        )

    def test_unwrap_fidl_forward_ref_from_locals(self) -> None:
        ty = ForwardRef("local_fnet.IpAddress")
        global fnet
        local_fnet = fnet
        self.assertEqual(
            fnet.IpAddress, unwrap_innermost_type(ty, localns=locals())
        )

    def test_unwrap_inception(self) -> None:
        ty = ForwardRef("tuple[ForwardRef('list[ForwardRef(\"str\")]')]")
        self.assertEqual(str, unwrap_innermost_type(ty, globalns=globals()))

    def test_unwrap_zx_type(self) -> None:
        ty = ForwardRef("zx.handle")
        self.assertEqual(int, unwrap_innermost_type(ty))

    def test_unwrap_no_wrapping(self) -> None:
        ty = str
        expected = str
        got = unwrap_innermost_type(ty)
        self.assertEqual(expected, got)

    def test_unwrap_multiple_type_arguments(self) -> None:
        with self.assertRaises(RuntimeError):
            unwrap_innermost_type(tuple[int, str])
        with self.assertRaises(RuntimeError):
            unwrap_innermost_type(tuple[tuple[int], str])

    def test_unwrap_bad_forward_ref(self) -> None:
        with self.assertRaises(NameError):
            unwrap_innermost_type(ForwardRef("foo"))

    def test_unwrap_bad_inception(self) -> None:
        with self.assertRaises(RuntimeError):
            unwrap_innermost_type(
                ForwardRef(
                    "tuple[ForwardRef('list[ForwardRef(\"tuple[int, str]\")]')]"
                ),
                globalns=globals(),
            )

    def test_get_type_from_import(self) -> None:
        # This import always expects things to be from "fidl.something.something" at a minimum, so
        # we're making an import with two levels.
        fooer_type = type("Fooer", (object,), {})
        mod = types.ModuleType(
            "foo.bar", "The foobinest foober that ever foob'd"
        )
        setattr(mod, "Fooer", fooer_type)
        sys.modules["foo.bar"] = mod
        expected = fooer_type
        got = get_type_from_import("foo.bar.Fooer")
        self.assertEqual(expected, got)

    def test_construct_response_object(self) -> None:
        raw_object = {"entry": [{"nodename": "foobar"}]}
        expected = ffx.TargetCollectionReaderNextRequest(
            entry=[ffx.TargetInfo(nodename="foobar")]
        )
        got = construct_response_object(
            "fuchsia.developer.ffx/TargetCollectionReaderNextRequest",
            raw_object,
        )
        self.assertEqual(expected, got)

    def test_construct_response_with_unions(self) -> None:
        raw_object = {
            "nodename": "foobar",
            "addresses": [
                {
                    "ip": {
                        "ip": {"ipv4": {"addr": [192, 168, 1, 1]}},
                        "scope_id": 3,
                    }
                }
            ],
        }
        ip = fnet.IpAddress()
        ip.ipv4 = fnet.Ipv4Address(addr=[192, 168, 1, 1])
        addrinfo = ffx.TargetAddrInfo()
        addrinfo.ip = ffx.TargetIp(ip=ip, scope_id=3)
        expected = ffx.TargetInfo(nodename="foobar", addresses=[addrinfo])
        got = construct_response_object(
            "fuchsia.developer.ffx/TargetInfo", raw_object
        )
        self.assertEqual(expected, got)

    def test_construct_response_empty_object(self) -> None:
        raw_object = None
        expected = None
        got = construct_response_object(None, raw_object)
        self.assertEqual(expected, got)

    def test_make_default_obj_nonetype(self) -> None:
        expected = None
        got = make_default_obj_from_ident(None)
        self.assertEqual(expected, got)

    def test_make_default_obj_composited_type(self) -> None:
        expected = ffx.TargetIp(ip=None, scope_id=None)
        got = make_default_obj_from_ident("fuchsia.developer.ffx/TargetIp")
        self.assertEqual(expected, got)
