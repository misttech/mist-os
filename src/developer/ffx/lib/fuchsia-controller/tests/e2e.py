# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import os
import os.path
import platform
import sys
import typing
import unittest

import fidl_fuchsia_developer_ffx as ffx_fidl
from fidl_codec import encode_fidl_message, method_ordinal
from fuchsia_controller_py import Channel, Context, IsolateDir

SDK_ROOT = "./sdk/exported/core"
# For Linux this handles the gamut of options.
# This map is derived from //build/rbe/fuchsia.py
PLATFORM_TYPE = {
    "x86_64": "x64",
    "arm64": "arm64",
}


def _locate_ffx_binary(sdk_manifest: str) -> str:
    """Locates the ffx binary from a given SDK root."""
    return os.path.join(
        SDK_ROOT,
        "tools",
        PLATFORM_TYPE[platform.machine()],
        "ffx",
    )


FFX_PATH = _locate_ffx_binary(SDK_ROOT)


class EndToEnd(unittest.IsolatedAsyncioTestCase):
    def _get_default_config(self) -> typing.Dict[str, str]:
        return {"sdk.root": SDK_ROOT}

    def _get_isolate_dir(self) -> IsolateDir:
        isolation_path = None
        tmp_path = os.getenv("TEST_UNDECLARED_OUTPUTS_DIR")
        if tmp_path:
            isolation_path = os.path.join(tmp_path, "isolate")
        return IsolateDir(dir=isolation_path)

    def _make_ctx(self):
        return Context(
            config=self._get_default_config(),
            isolate_dir=self._get_isolate_dir(),
        )

    def test_config_get_nonexistent(self):
        ctx = self._make_ctx()
        self.assertEqual(ctx.config_get_string("foobarzzzzzzo==?"), None)

    def test_config_get_exists(self):
        config = self._get_default_config()
        key = "foobar"
        expect = "bazmumble"
        config[key] = expect
        ctx = Context(config=config, isolate_dir=self._get_isolate_dir())
        self.assertEqual(ctx.config_get_string(key), expect)

    def test_config_get_too_long(self):
        config = self._get_default_config()
        key = "foobarzington"
        expect = "b" * 50000
        config[key] = expect
        ctx = Context(config=config, isolate_dir=self._get_isolate_dir())
        with self.assertRaises(BufferError):
            ctx.config_get_string(key)

    async def test_client_sends_message_before_coro_await(self):
        (ch0, ch1) = Channel.create()
        echo_proxy = ffx_fidl.EchoClient(ch0)
        coro = echo_proxy.echo_string(value="foo")
        buf, _ = ch1.read()
        txid = int.from_bytes(buf[0:4], sys.byteorder)
        encoded_bytes, _ = encode_fidl_message(
            object=ffx_fidl.EchoEchoStringRequest(value="foo"),
            library="fuchsia.developer.ffx",
            type_name="fuchsia.developer.ffx/EchoEchoStringRequest",
            txid=txid,
            ordinal=method_ordinal(
                protocol="fuchsia.developer.ffx/Echo", method="EchoString"
            ),
        )
        self.assertEqual(buf, encoded_bytes)
        msg = encode_fidl_message(
            object=ffx_fidl.EchoEchoStringResponse(response="otherthing"),
            library="fuchsia.developer.ffx",
            type_name="fuchsia.developer.ffx/EchoEchoStringResponse",
            txid=txid,
            ordinal=method_ordinal(
                protocol="fuchsia.developer.ffx/Echo", method="EchoString"
            ),
        )
        ch1.write(msg)
        result = await coro
        self.assertEqual(result.response, "otherthing")

    def test_context_creation_duplicate_target_raises_exception(self):
        with self.assertRaises(RuntimeError):
            _ctx = Context(target="foo", config={"target.default": "bar"})

    def test_context_creation_no_args(self):
        Context()

    async def test_sending_fidl_protocol(self):
        tc_server, tc_client = Channel.create()
        list_server, list_client = Channel.create()
        tc_proxy = ffx_fidl.TargetCollectionClient(tc_client)
        query = ffx_fidl.TargetQuery(string_matcher="foobar")
        tc_proxy.list_targets(query=query, reader=list_client.take())
        buf, hdls = tc_server.read()
        self.assertEqual(len(hdls), 1)
        new_list_client = Channel(hdls[0])
        new_list_client.write((bytearray([5, 6, 7]), []))
        list_buf, list_hdls = list_server.read()
        self.assertEqual(len(list_hdls), 0)
        self.assertEqual(list_buf, bytearray([5, 6, 7]))
