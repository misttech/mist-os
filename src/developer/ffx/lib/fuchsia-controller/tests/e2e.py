# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import unittest
import fidl.fuchsia_developer_ffx as ffx_fidl
import os
import tempfile
import os.path
import asyncio
from fuchsia_controller_py import Context, IsolateDir


class EndToEnd(unittest.IsolatedAsyncioTestCase):

    def _make_ctx(self):
        isolation_path = None
        tmp_path = os.getenv("TEST_UNDECLARED_OUTPUTS_DIR")
        if tmp_path:
            isolation_path = os.path.join(tmp_path, "isolate")

        return Context(
            config={"sdk.root": "."},
            isolate_dir=IsolateDir(dir=isolation_path))

    async def test_echo_daemon(self):
        ctx = self._make_ctx()
        echo_proxy = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER))
        expected = "this is an echo test"
        result = await echo_proxy.echo_string(value=expected)
        self.assertEqual(result.response, expected)

    async def test_echo_daemon_parallel(self):
        ctx = self._make_ctx()
        echo_proxy = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER))
        expected1 = "this is an echo test1"
        expected2 = "22222this is an echo test2"
        expected3 = "frobination incoming. Heed the call of the frobe"
        loop = asyncio.get_running_loop()
        result1 = loop.create_task(echo_proxy.echo_string(value=expected1))
        result2 = loop.create_task(echo_proxy.echo_string(value=expected2))
        result3 = loop.create_task(echo_proxy.echo_string(value=expected3))
        results_list = await asyncio.gather(result1, result2, result3)
        self.assertEqual(results_list[0].response, expected1)
        self.assertEqual(results_list[1].response, expected2)
        self.assertEqual(results_list[2].response, expected3)
        self.assertEqual(len(echo_proxy.pending_txids), 0)
        self.assertEqual(len(echo_proxy.staged_messages), 0)

    def test_context_creation_no_config_but_target(self):
        """This test simply ensures passing a target does not cause an error."""
        _ctx = Context(target="foo")

    def test_context_creation_duplicate_target_raises_exception(self):
        with self.assertRaises(RuntimeError):
            _ctx = Context(target="foo", config={"target.default": "bar"})

    def test_context_creation_no_args(self):
        _ctx = Context()

    def test_setting_fidl_clients(self):
        """Previously a classmethod was setting the handle.

        This ensures these aren't being set globally."""
        ctx = self._make_ctx()
        e1 = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER))
        e2 = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER))
        self.assertNotEqual(e1.channel.as_int(), e2.channel.as_int())
