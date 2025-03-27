# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

import fidl_test_python_otherstruct as test_python_otherstruct
import fidl_test_python_struct as test_python_struct
from fidl import construct_response_object

# TODO(https://fxbug.dev/346628306): Enable type checking here once fidl_codec has stubs
from fidl_codec import decode_standalone  # type: ignore


class ExternalLibraryTestsuite(unittest.TestCase):
    def test_creation_of_struct_containing_external_struct(self) -> None:
        a = test_python_struct.StructWithExternalStructField(
            value=test_python_otherstruct.EmptyStruct()
        )
        b = test_python_struct.StructWithExternalStructField(
            value=test_python_otherstruct.EmptyStruct()
        )
        self.assertEqual(a, b)

    def test_encode_external_struct_in_struct(self) -> None:
        value = test_python_struct.StructWithExternalStructField(
            value=test_python_otherstruct.EmptyStruct()
        )
        encoded_bytes, hdls = value.encode()
        # fmt: off
        self.assertEqual(encoded_bytes, bytearray([0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00]))
        # fmt: on
        self.assertEqual(hdls, [])

    def test_decode_external_struct_in_struct(self) -> None:
        handles: list[int] = []
        # fmt: off
        encoded_bytes = bytearray([0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00])
        # fmt: on
        type_name = "test.python.struct/StructWithExternalStructField"
        value = decode_standalone(
            type_name=type_name, bytes=encoded_bytes, handles=handles
        )
        value = construct_response_object(type_name, value)
        self.assertEqual(
            value,
            test_python_struct.StructWithExternalStructField(
                value=test_python_otherstruct.EmptyStruct()
            ),
        )
