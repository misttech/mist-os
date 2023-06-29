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

    def test_context_creation_no_config_but_target(self):
        """This test simply ensures passing a target does not cause an error."""
        _ctx = Context(target="foo")

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
        self.assertNotEqual(e1.handle.as_int(), e2.handle.as_int())
