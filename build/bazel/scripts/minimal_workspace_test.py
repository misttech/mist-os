#!/usr/bin/env python3
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import tempfile
import unittest
from pathlib import Path
from unittest import mock

import minimal_workspace
import update_workspace

_FAKE_BUILD_CONFIG = {
    "host_tag": "powerpc",
    "rbe_instance_name": "johnny/cache/instance/default",
    "rbe_project": "cache-is-king",
}


class MainTests(unittest.TestCase):
    def test_generate(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            # Args such as --fuchsia-dir, --bazel-bin can point to
            # arbitrary/default locations because this test will not
            # actually try to access those paths.
            with mock.patch.object(
                Path, "read_text", return_value="template text\n"
            ) as mock_read:
                with mock.patch.object(
                    update_workspace,
                    "generate_fuchsia_build_config",
                    return_value=_FAKE_BUILD_CONFIG,
                ) as mock_build_config:
                    status = minimal_workspace.main([f"--topdir={td}"])

            self.assertEqual(status, 0)
            mock_build_config.assert_called_once()
            mock_read.assert_called()  # twice, once per template file


if __name__ == "__main__":
    unittest.main()
