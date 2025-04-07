# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import typing
import unittest
from typing import ForwardRef, Optional, Sequence, Union

import fidl_fuchsia_developer_ffx as ffx
import fidl_fuchsia_net as fnet
from fidl._construct import (
    construct_response_object,
    make_default_obj_from_ident,
    unwrap_innermost_type,
)


class Construct(unittest.TestCase):
    """Tests for constructing Python objects from decoded FIDL messages"""

    def test_unwrap_multiple_layers(self) -> None:
        ty = typing.cast(type, Optional[Sequence[str]])
        self.assertEqual(str, unwrap_innermost_type(ty))

    def test_unwrap_simple_forward_ref(self) -> None:
        ty = ForwardRef("str")
        self.assertEqual(str, unwrap_innermost_type(ty))

    def test_unwrap_optional(self) -> None:
        ty = typing.cast(type, Optional[str])
        self.assertEqual(str, unwrap_innermost_type(ty))

    def test_unwrap_union_equivalent_optional(self) -> None:
        ty = typing.cast(type, Union[int, None])
        self.assertEqual(int, unwrap_innermost_type(ty))

    def test_unwrap_union_type_equivalent_optional(self) -> None:
        ty = typing.cast(type, float | None)
        self.assertEqual(float, unwrap_innermost_type(ty))

    def test_unwrap_bad_union(self) -> None:
        ty = typing.cast(type, float | int | None)
        with self.assertRaises(RuntimeError):
            unwrap_innermost_type(ty)

    def test_unwrap_fidl_forward_ref(self) -> None:
        ty = ForwardRef("IpAddress", module=fnet.__name__)
        self.assertEqual(fnet.IpAddress, unwrap_innermost_type(ty))

    def test_unwrap_inception(self) -> None:
        ty = ForwardRef("tuple[ForwardRef('list[ForwardRef(\"str\")]')]")
        self.assertEqual(str, unwrap_innermost_type(ty))

    def test_unwrap_zx_type(self) -> None:
        ty = ForwardRef("zx.handle")
        self.assertEqual(int, unwrap_innermost_type(ty))

    def test_unwrap_no_wrapping(self) -> None:
        self.assertEqual(str, unwrap_innermost_type(str))

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
            )

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

    def test_construct_response_with_nested_unions(self) -> None:
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
        ip = fnet.IpAddress(ipv4=fnet.Ipv4Address(addr=[192, 168, 1, 1]))
        addrinfo = ffx.TargetAddrInfo(ip=ffx.TargetIp(ip=ip, scope_id=3))
        expected = ffx.TargetInfo(nodename="foobar", addresses=[addrinfo])
        got = construct_response_object(
            "fuchsia.developer.ffx/TargetInfo", raw_object
        )
        self.assertEqual(expected, got)

    def test_make_default_obj_composited_type(self) -> None:
        expected = ffx.TargetIp(ip=None, scope_id=None)  # type: ignore[arg-type]
        got = make_default_obj_from_ident("fuchsia.developer.ffx/TargetIp")
        self.assertEqual(expected, got)
