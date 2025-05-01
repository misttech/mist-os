# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Import needed for tests to work
import typing
import unittest  # NOQA

import common
from fidl_codec import (  # type: ignore # we don't have types for this cpp file
    decode_fidl_request,
    encode_fidl_message,
    method_ordinal,
)


class EncodeObj(object):
    """Just a generic object to be used for encoding tests."""


class Encode(common.FuchsiaControllerTest):
    """Fuchsia Controller FIDL Encoding Tests"""

    def test_encode_noop_function(self) -> None:
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

    def test_encode_struct_missing_field(self) -> None:
        with self.assertRaises(AttributeError):
            encode_fidl_message(
                object=object(),
                library="fuchsia.controller.test",
                type_name="fuchsia.controller.test/NoopDoStringNoopRequest",
                txid=1,
                ordinal=12345,
            )

    def test_encode_non_nullable_value(self) -> None:
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

    def test_encode_table_and_union(self) -> None:
        EXPECTED = bytearray.fromhex(
            "4d00000002000001b8220000000000000300000000000000ffffffffffffffff0000004000000100180000000000000018000000000000000600000000000000ffffffffffffffff666f6f6261720000030000000000000008000000000000000500000000000000"
        )
        obj = typing.cast(typing.Any, EncodeObj())
        setattr(obj, "tab", EncodeObj())
        setattr(obj.tab, "dub", 2.0)
        setattr(obj.tab, "str_", "foobar")
        setattr(obj.tab, "union_field", EncodeObj())
        setattr(obj.tab.union_field, "union_int", 5)

        # This intentionally omits the integer field rather than explicitly setting it to None.
        # This test covers that the error bit is successfully cleared when converting a table to
        # something where an attribute is missing rather than set to None. Without clearing the
        # error properly this will raise an exception. This is the same reason union_int is used
        # above. When iterating over the object, "union_str" will be checked first, which will
        # return nullptr and set a "AttributeError" exception.
        def encode_fn(x: typing.Any) -> typing.Any:
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

    def test_encode_decode_table_and_union(self) -> None:
        obj = typing.cast(typing.Any, EncodeObj())
        setattr(obj, "tab", EncodeObj())
        setattr(obj.tab, "dub", 2.0)
        setattr(obj.tab, "str_", "foobar")
        setattr(obj.tab, "union_field", EncodeObj())
        setattr(obj.tab.union_field, "union_int", 5)
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoTableNoopRequest",
            txid=5,
            ordinal=method_ordinal(
                protocol="fuchsia.controller.test/Noop", method="DoTableNoop"
            ),
        )

        self.assertEqual(h, [])
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(
            msg,
            {
                "tab": {
                    "dub": 2.0,
                    "integer": None,
                    "str_": "foobar",
                    "union_field": {
                        "union_int": 5,
                    },
                }
            },
        )

    def test_encode_handle(self) -> None:
        EXPECTED = bytearray.fromhex(
            "7b000000020000010f27000000000000ffffffff00000000"
        )
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
        self.assertEqual(len(h), 1)
        self.assertEqual(h[0][1], 5)

    def test_encode_decode_handle(self) -> None:
        obj = EncodeObj()
        setattr(obj, "server_end", 10)
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoHandleNoopRequest",
            txid=22,
            ordinal=method_ordinal(
                protocol="fuchsia.controller.test/Noop", method="DoHandleNoop"
            ),
        )
        updated_handles = [hdl[1] for hdl in h]
        msg = decode_fidl_request(bytes=b, handles=updated_handles)
        self.assertEqual(msg, {"server_end": 10})

    def test_encode_string_vector(self) -> None:
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

    def test_handle_overflow(self) -> None:
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

    def test_bits_overflow(self) -> None:
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

    def test_encode_fail_missing_params(self) -> None:
        with self.assertRaises(TypeError):
            encode_fidl_message(
                object=None, library=None, type_name=None, ordinal=None
            )

    def test_encode_method_call_no_args(self) -> None:
        (b, h) = encode_fidl_message(
            object=None, library=None, type_name=None, txid=123, ordinal=345
        )

    def test_encode_decode_string_vector(self) -> None:
        obj = Encode()
        setattr(obj, "v", ["foo", "bar", "baz", "qux"])
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoVectorNoopRequest",
            txid=12345,
            ordinal=method_ordinal(
                protocol="fuchsia.controller.test/Noop", method="DoVectorNoop"
            ),
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"v": ["foo", "bar", "baz", "qux"]})

    def test_encode_decode_string_vector_with_sequence(self) -> None:
        """
        We accept not just lists, but also more generally sequences as input
        for FIDL arrays and vectors. Strings themselves, in Python, are also
        sequences of bytes. The docs say that since there is no separate
        "character" class, strings of length 1 are used to represent
        characters. Therefore, the expected outcome should be that we accept
        a string for a vector, and chunk it into its individual characters
        in FIDL.
        Ref: https://docs.python.org/3/library/stdtypes.html#textseq
        """
        obj = Encode()
        setattr(obj, "v", "sequence")
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoVectorNoopRequest",
            txid=12345,
            ordinal=method_ordinal(
                protocol="fuchsia.controller.test/Noop", method="DoVectorNoop"
            ),
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"v": ["s", "e", "q", "u", "e", "n", "c", "e"]})

    def test_encode_decode_int_array_with_list(self) -> None:
        obj = Encode()
        setattr(obj, "a", [1, 2, 3, 4])
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoArrayNoopRequest",
            txid=12345,
            ordinal=method_ordinal(
                protocol="fuchsia.controller.test/Noop", method="DoArrayNoop"
            ),
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"a": [1, 2, 3, 4]})

    def test_encode_decode_int_array_with_sequence(self) -> None:
        """
        We accept not just lists, but also more generally sequences as input
        for FIDL arrays and vectors. The `bytes` type is a common example of
        a sequence that is _not_ a list, so use it in the test here.
        """
        obj = Encode()
        setattr(obj, "a", bytes([1, 2, 3, 4]))
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoArrayNoopRequest",
            txid=12345,
            ordinal=method_ordinal(
                protocol="fuchsia.controller.test/Noop", method="DoArrayNoop"
            ),
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"a": [1, 2, 3, 4]})

    def test_encode_decode_string(self) -> None:
        obj = EncodeObj()
        setattr(obj, "value", "foobar")
        ordinal = method_ordinal(
            protocol="fuchsia.controller.test/Noop", method="DoStringNoop"
        )
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoStringNoopRequest",
            txid=3,
            ordinal=ordinal,
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"value": "foobar"})

    def test_encode_decode_int(self) -> None:
        obj = EncodeObj()
        setattr(obj, "value", 2)
        ordinal = method_ordinal(
            protocol="fuchsia.controller.test/Noop", method="DoIntNoop"
        )
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.test",
            type_name="fuchsia.controller.test/NoopDoIntNoopRequest",
            txid=3,
            ordinal=ordinal,
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"value": 2})

    def test_encode_decode_negative_enum(self) -> None:
        obj = EncodeObj()
        # This is a sepcial case because there is also a positive 2 represented in this enum.
        # without a negative value check in the encoder path, this test will fail.
        setattr(obj, "enum_thing", -2)
        ordinal = method_ordinal(
            protocol="fuchsia.controller.othertest/CrossLibraryNoop",
            method="EnumMethod",
        )
        (b, h) = encode_fidl_message(
            object=obj,
            library="fuchsia.controller.othertest",
            type_name="fuchsia.controller.othertest/CrossLibraryNoopEnumMethodRequest",
            txid=5,
            ordinal=ordinal,
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg, {"enum_thing": -2})
