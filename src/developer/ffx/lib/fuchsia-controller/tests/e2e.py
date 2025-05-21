# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import asyncio
import os
import os.path
import sys
import typing
import unittest

import fidl_fuchsia_controller_test as fc_test
import fidl_fuchsia_developer_ffx as ffx_fidl
from fidl_codec import encode_fidl_message, method_ordinal
from fuchsia_controller_py import Channel, Context, IsolateDir, Socket, ZxStatus

from fidl import DomainError


class TestSocketServer(fc_test.TestingServer):
    def __init__(self, ch: Channel):
        client, server = Socket.create()
        self.client = client
        self.server = server
        super().__init__(ch)

    def return_socket_or_error(
        self,
    ) -> fc_test.TestingServer.ReturnSocketOrErrorResponse:
        if self.server.as_int() == 0:
            return DomainError(ZxStatus.ZX_ERR_NOT_SUPPORTED)
        return fc_test.TestingReturnSocketOrErrorResponse(
            reader_thing=self.server.take()
        )

    def return_union(self) -> fc_test.TestingServer.ReturnUnionResponse:
        res = fc_test.TestingReturnUnionResponse(x=10)
        return res

    def return_union_with_table(
        self,
    ) -> fc_test.TestingServer.ReturnUnionWithTableResponse:
        x = fc_test.NoopUnion(union_str="foobar")
        res = fc_test.TestingReturnUnionWithTableResponse(x=x)
        return res

    def return_other_composed_protocol(
        self,
    ) -> fc_test.TestingServer.ReturnOtherComposedProtocolResponse:
        return fc_test.TestingReturnOtherComposedProtocolResponse(
            client_thing=0
        )

    def return_possible_error(
        self,
    ) -> fc_test.TestingServer.ReturnPossibleErrorResponse:
        return DomainError("not implemented")

    def return_possible_error2(
        self,
    ) -> fc_test.TestingServer.ReturnPossibleError2Response:
        return DomainError("not implemented")


class EndToEnd(unittest.IsolatedAsyncioTestCase):
    def _get_default_config(self) -> typing.Dict[str, str]:
        return {}

    def _get_isolate_dir(self) -> IsolateDir:
        isolation_path = None
        tmp_path = os.getenv("TEST_UNDECLARED_OUTPUTS_DIR")
        if tmp_path:
            isolation_path = os.path.join(tmp_path, "isolate")
        return IsolateDir(dir=isolation_path)

    def _make_ctx(self) -> Context:
        return Context(
            config=self._get_default_config(),
            isolate_dir=self._get_isolate_dir(),
        )

    def test_config_get_nonexistent(self) -> None:
        ctx = self._make_ctx()
        self.assertEqual(ctx.config_get_string("foobarzzzzzzo==?"), None)

    def test_config_get_exists(self) -> None:
        config = self._get_default_config()
        key = "foobar"
        expect = "bazmumble"
        config[key] = expect
        ctx = Context(config=config, isolate_dir=self._get_isolate_dir())
        self.assertEqual(ctx.config_get_string(key), expect)

    def test_config_get_too_long(self) -> None:
        config = self._get_default_config()
        key = "foobarzington"
        expect = "b" * 50000
        config[key] = expect
        ctx = Context(config=config, isolate_dir=self._get_isolate_dir())
        with self.assertRaises(BufferError):
            ctx.config_get_string(key)

    async def test_client_sends_message_before_coro_await(self) -> None:
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

    def test_context_creation_duplicate_target_raises_exception(self) -> None:
        with self.assertRaises(RuntimeError):
            _ctx = Context(target="foo", config={"target.default": "bar"})

    def test_context_creation_no_args(self) -> None:
        Context()

    async def test_sending_fidl_protocol(self) -> None:
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

    async def test_sending_socket_as_result(self) -> None:
        t_server, t_client = Channel.create()
        server = TestSocketServer(t_server)
        client = fc_test.TestingClient(t_client)
        server_task = asyncio.get_running_loop().create_task(server.serve())
        res1 = await client.return_union()
        self.assertEqual(res1.x, 10)
        res2 = await client.return_union_with_table()
        # This is to placate mypy
        assert res2.x is not None
        self.assertEqual(res2.x.union_str, "foobar")
        res3 = await client.return_socket_or_error()
        # TODO(b/415379365): Given this is intended to be a socket, it should return
        # the appropriate type here.
        assert res3.response is not None
        self.assertNotEqual(res3.response.reader_thing, 0)
        res4 = await client.return_socket_or_error()
        self.assertEqual(res4.err, ZxStatus.ZX_ERR_NOT_SUPPORTED)
        server_task.cancel()
        server.close()
