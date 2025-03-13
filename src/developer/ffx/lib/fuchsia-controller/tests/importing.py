# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import importlib
import unittest

from fidl_codec import (  # type: ignore[attr-defined]
    decode_fidl_request,
    decode_standalone,
    encode_fidl_message,
    method_ordinal,
)


class Importing(unittest.TestCase):
    """Fuchsia Controller FIDL Encoding Tests"""

    def test_import_everything(self) -> None:
        # For now import the existing fidl_codec test libraries to ensure importing functions properly.
        libs = [
            "test.fidlcodec.examples",
            "test.fidlcodec.composedinto",
            "test.fidlcodec.sys",
        ]
        for library in libs:
            fidl_library = library.replace(".", "_")
            importlib.import_module("fidl." + fidl_library)

    def test_construct_objects(self) -> None:
        mod = importlib.import_module("fidl.test_fidlcodec_examples")
        s = mod.Named(s="foobar")
        self.assertEqual(s.s, "foobar")
        _request = mod.FidlCodecXUnionSendAfterMigrationRequest(
            u=mod.NowAsXUnion.variant_u8_type(5), i=10
        )
        _request2 = mod.FidlCodecTestProtocolStringRequest(s="foobar")
        _request3 = mod.FidlCodecTestProtocolNullableXUnionRequest(
            isu=None, i=10
        )

    def test_encode_union_request(self) -> None:
        mod = importlib.import_module("fidl.test_fidlcodec_examples")
        isu = mod.IntStructXunion()
        isu.variant_tss = isu.variant_tss_type(value1="foo", value2="bar")
        request = mod.FidlCodecTestProtocolNullableXUnionRequest(isu=isu, i=10)
        (b, h) = encode_fidl_message(
            object=request,
            library="test.fidlcodec.examples",
            type_name="test.fidlcodec.examples/FidlCodecTestProtocolNullableXUnionRequest",
            txid=1,
            ordinal=method_ordinal(
                protocol="test.fidlcodec.examples/FidlCodecTestProtocol",
                method="NullableXUnion",
            ),
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg["isu"]["variant_tss"]["value1"], "foo")
        self.assertEqual(msg["isu"]["variant_tss"]["value2"], "bar")
        self.assertEqual(msg["i"], 10)

    def test_import_cross_library(self) -> None:
        mod = importlib.import_module("fidl.fuchsia_controller_othertest")
        other_mod = importlib.import_module("fidl.fuchsia_controller_test")
        v = other_mod.NoopUnion()
        v.union_str = v.union_str_type("foooberdooberdoo")
        _s = mod.CrossLibraryStruct(value=v)

    def test_encode_decode_enum_message(self) -> None:
        mod = importlib.import_module("fidl.test_fidlcodec_examples")
        req = mod.FidlCodecTestProtocolDefaultEnumMessageRequest(
            ev=mod.DefaultEnum.X
        )
        (b, h) = encode_fidl_message(
            object=req,
            library="test.fidlcodec.examples",
            type_name="test.fidlcodec.examples/FidlCodecTestProtocolDefaultEnumMessageRequest",
            txid=1,
            ordinal=method_ordinal(
                protocol="test.fidlcodec.examples/FidlCodecTestProtocol",
                method="DefaultEnumMessage",
            ),
        )
        msg = decode_fidl_request(bytes=b, handles=h)
        self.assertEqual(msg["ev"], mod.DefaultEnum.X)

    def test_alias_import_subclasses_structs(self) -> None:
        mod = importlib.import_module("fidl.test_fidlcodec_examples")
        alias = mod.NamedAlias
        base_ty = mod.Named
        self.assertTrue(issubclass(alias, base_ty))

    def test_alias_type_correct_base(self) -> None:
        mod = importlib.import_module("fidl.test_fidlcodec_examples")
        alias = mod.StringAlias
        self.assertTrue(issubclass(alias, str))
        alias = mod.VectorAlias
        self.assertTrue(issubclass(alias, list))

    def test_alias_from_other_library_is_imported(self) -> None:
        mod = importlib.import_module("fidl.test_fidlcodec_examples")
        alias = mod.HandleAlias
        self.assertTrue(issubclass(alias, int))

    def test_possible_error_method(self) -> None:
        mod = importlib.import_module("fidl.fuchsia_controller_test")
        # Simply setting this should be enough of a test, as it informs that the type was
        # constructed properly.
        res = mod.ComposerThingReturnPossibleErrorResult()
        res.err = 5

    def test_protocol_classes_exist(self) -> None:
        mod = importlib.import_module("fidl.fuchsia_controller_test")
        self.assertEqual(mod.NoopClient, mod.Noop.Client)
        self.assertEqual(mod.NoopServer, mod.Noop.Server)
        self.assertEqual(mod.NoopEventHandler, mod.Noop.EventHandler)

    def test_encode_protocol_return_value(self) -> None:
        """This test covers a problem area with naming protocol methods in IR.

        What happens, in this test, is that if we allocate
        `ComposerThing.ReturnPossibleError`, we'll have to make sure we're
        encoding an object like `ComposerThingReturnPossibleError2Result`
        translates FIDL IR lookup to be
        `ComposerThing_ReturnPossibleError2_Result` and vice versa.

        It's unclear if this test should be here or encode.py, since it's doing
        both things, but it's here for now.
        """
        mod = importlib.import_module("fidl.fuchsia_controller_test")
        obj = mod.ComposerThingReturnPossibleError2Result()
        obj.err = obj.err_type(2)
        (b, h) = obj.encode()
        msg = decode_standalone(
            type_name=obj.__fidl_raw_type__, bytes=b, handles=h
        )
        self.assertEqual(msg, {"err": 2})
