# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import sys
import types
import typing
import fidl.fuchsia_developer_ffx as ffx
import fidl.fuchsia_net as fnet
from fidl._fidl_common import *


class FidlCommon(unittest.TestCase):
    """Tests for FIDL common utils"""

    def test_camel_case_to_snake_case_standard(self):
        expected = "test_string"
        got = camel_case_to_snake_case("TestString")
        self.assertEqual(expected, got)

    def test_camel_case_to_snake_case_empty_string(self):
        expected = ""
        got = camel_case_to_snake_case("")
        self.assertEqual(expected, got)

    def test_camel_case_to_snake_case_all_caps(self):
        expected = "f_f_f_f"
        got = camel_case_to_snake_case("FFFF")
        self.assertEqual(expected, got)

    def test_unwrap_type_multiple_layers(self):
        ty = typing.Optional[typing.Sequence[str]]
        expected = str
        got = unwrap_type(ty)
        self.assertEqual(expected, got)

    def test_unwrap_type_forward_ref(self):
        ty = typing.ForwardRef("fooberdoo")
        expected = "fooberdoo"
        got = unwrap_type(ty)
        self.assertEqual(expected, got)

    def test_unwrap_type_no_wrapping(self):
        ty = str
        expected = str
        got = unwrap_type(ty)
        self.assertEqual(expected, got)

    def test_get_type_from_import(self):
        # This import always expects things to be from "fidl.something.something" at a minimum, so
        # we're making an import with two levels.
        fooer_type = type("Fooer", (object,), {})
        mod = types.ModuleType(
            'foo.bar', "The foobinest foober that ever foob'd")
        setattr(mod, "Fooer", fooer_type)
        sys.modules["foo.bar"] = mod
        expected = fooer_type
        got = get_type_from_import("foo.bar.Fooer")
        self.assertEqual(expected, got)

    def test_construct_response_object(self):
        raw_object = {"entry": [{"nodename": "foobar"}]}
        expected = ffx.TargetCollectionReaderNextRequest(
            entry=[ffx.TargetInfo(nodename="foobar")])
        got = construct_response_object(
            "fuchsia.developer.ffx/TargetCollectionReaderNextRequest",
            raw_object)
        self.assertEqual(expected, got)

    def test_construct_response_with_unions(self):
        raw_object = {
            "nodename":
                "foobar",
            "addresses":
                [
                    {
                        "ip":
                            {
                                "ip": {
                                    "ipv4": {
                                        "addr": [192, 168, 1, 1]
                                    }
                                },
                                "scope_id": 3
                            }
                    }
                ]
        }
        ip = fnet.IpAddress()
        ip.ipv4 = fnet.Ipv4Address(addr=[192, 168, 1, 1])
        addrinfo = ffx.TargetAddrInfo()
        addrinfo.ip = ffx.TargetIp(ip=ip, scope_id=3)
        expected = ffx.TargetInfo(nodename="foobar", addresses=[addrinfo])
        got = construct_response_object(
            "fuchsia.developer.ffx/TargetInfo", raw_object)
        self.assertEqual(expected, got)

    def test_construct_response_empty_object(self):
        raw_object = None
        expected = None
        got = construct_response_object(None, raw_object)
        self.assertEqual(expected, got)

    def test_make_default_obj_nonetype(self):
        expected = None
        got = make_default_obj_from_ident(None)
        self.assertEqual(expected, got)

    def test_make_default_obj_composited_type(self):
        expected = ffx.TargetIp(ip=None, scope_id=None)
        got = make_default_obj_from_ident("fuchsia.developer.ffx/TargetIp")
        self.assertEqual(expected, got)
