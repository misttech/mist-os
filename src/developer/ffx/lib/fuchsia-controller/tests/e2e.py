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

    @classmethod
    def setUpClass(self):
        path = os.getenv("TEST_UNDECLARED_OUTPUTS_DIR")
        if path:
            self.isolation_path = os.path.join(path, "isolate")
        else:
            tmpdir = tempfile.mkdtemp()
            self.isolation_path = str(tmpdir)

    def _make_ctx(self):
        return Context(
            config={"sdk.root": "."},
            isolate_dir=IsolateDir(self.isolation_path))

    async def test_echo_daemon(self):
        ctx = self._make_ctx()
        echo_proxy = ffx_fidl.Echo.Client(
            ctx.connect_daemon_protocol(ffx_fidl.Echo.MARKER))
        expected = "this is an echo test"
        result = await echo_proxy.echo_string(value=expected)
        self.assertEqual(result.response, expected)
