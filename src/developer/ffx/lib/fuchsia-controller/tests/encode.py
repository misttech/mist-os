# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from fidl_codec import encode_fidl_message, add_ir_path
import common
import unittest


class EncodeObj(object):
    """Just a generic object to be used for encoding tests."""

    pass


class Encode(common.FuchsiaControllerTest):
    """Fuchsia Controller FIDL Encoding Tests"""

    def test_encode_noop_function(self):
        EXPECTED = bytearray.fromhex(
            "020000000200000139300000000000000600000000000000ffffffffffffffff666f6f6261720000"
        )
        obj = EncodeObj()
        setattr(obj, "value", "foobar")
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoStringNoopRequest",
            txid=2,
            ordinal=12345,
        )
        self.assertEqual(b, EXPECTED)
        self.assertEqual(h, [])

    def test_encode_struct_missing_field(self):
        with self.assertRaises(AttributeError):
            encode_fidl_message(
                object=object(),
                library="fuchsia.controller.test",
                type_name="fuchsia.controller.test/NoopDoStringNoopRequest",
                txid=1,
                ordinal=12345,
            )

    def test_encode_non_nullable_value(self):
        obj = EncodeObj()
        setattr(obj, "value", None)
        with self.assertRaises(TypeError):
            encode_fidl_message(
                object=obj,
                library="fuchsia.controller.test",
                type_name="fuchsia.controller.test/NoopDoIntNoopRequest",
                txid=3,
                ordinal=12345,
            )

    def test_encode_table_and_union(self):
        EXPECTED = bytearray.fromhex(
            "4d00000002000001b8220000000000000300000000000000ffffffffffffffff0000004000000100180000000000000018000000000000000600000000000000ffffffffffffffff666f6f6261720000030000000000000008000000000000000500000000000000"
        )
        obj = EncodeObj()
        setattr(obj, "tab", EncodeObj())
        setattr(obj.tab, "dub", 2.0)
        setattr(obj.tab, "str", "foobar")
        setattr(obj.tab, "union_field", EncodeObj())
        setattr(obj.tab.union_field, "union_int", 5)

        # This intentionally omits the integer field rather than explicitly setting it to None.
        # This test covers that the error bit is successfully cleared when converting a table to
        # something where an attribute is missing rather than set to None. Without clearing the
        # error properly this will raise an exception. This is the same reason union_int is used
        # above. When iterating over the object, "union_str" will be checked first, which will
        # return nullptr and set a "AttributeError" exception.
        def encode_fn(x):
            return encode_fidl_message(
                object=x,
                library="fuchsia.controller.test",
                type_name="fuchsia.controller.test/NoopDoTableNoopRequest",
                txid=77,
                ordinal=8888,
            )

        (b, h) = encode_fn(obj)
        self.assertEqual(b, EXPECTED)
        self.assertEqual(h, [])

        # Encode again with an explicit None to verify the behavior is the same as omitting a field
        # altogether.
        setattr(obj.tab, "integer", None)
        (b, h) = encode_fn(obj)
        self.assertEqual(b, EXPECTED)
        self.assertEqual(h, [])

    def test_encode_handle(self):
        EXPECTED = bytearray.fromhex(
            "7b000000020000010f27000000000000ffffffff00000000")
        obj = EncodeObj()
        setattr(obj, "server_end", 5)
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoHandleNoopRequest",
            txid=123,
            ordinal=9999,
        )
        self.assertEqual(b, EXPECTED)
        self.assertEqual(h, [5])

    def test_encode_string_vector(self):
        EXPECTED = bytearray.fromhex(
            "3930000002000001b3150000000000000400000000000000ffffffffffffffff0300000000000000ffffffffffffffff0300000000000000ffffffffffffffff0300000000000000ffffffffffffffff0300000000000000ffffffffffffffff666f6f0000000000626172000000000062617a00000000007175780000000000"
        )
        obj = Encode()
        setattr(obj, "v", ["foo", "bar", "baz", "qux"])
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoVectorNoopRequest",
            txid=12345,
            ordinal=5555,
        )
        self.assertEqual(b, EXPECTED)
        self.assertEqual(h, [])

    def test_handle_overflow(self):
        obj = Encode()
        # Should be too large for a u32.
        setattr(obj, "server_end", 0xFFFFFFFFFF)
        with self.assertRaises(OverflowError):
            encode_fidl_message(
                object=obj,
                library="fuchsia.controller.test",
                type_name="fuchsia.controller.test/NoopDoHandleNoopRequest",
                txid=1234,
                ordinal=5555,
            )

    def test_bits_overflow(self):
        obj = Encode()
        setattr(obj, "b", 0xFFFFFFFFFFFFFFFFFFFFFF)
        with self.assertRaises(OverflowError):
            encode_fidl_message(
                object=obj,
                library="fuchsia.controller.test",
                type_name="fuchsia.controller.test/NoopDoBitsNoopRequest",
                txid=2222,
                ordinal=3333,
            )

    def test_encode_fail_missing_params(self):
        with self.assertRaises(TypeError):
            encode_fidl_message(
                object=None, library=None, type_name=None, ordinal=None)

    def test_encode_method_call_no_args(self):
        (b, h) = encode_fidl_message(
            object=None, library=None, type_name=None, txid=123, ordinal=345)
