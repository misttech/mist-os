#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(__file__))
import workspace_utils


class TestGetBazelRelativeTopDir(unittest.TestCase):
    def test_get_bazel_relative_topdir(self):
        with tempfile.TemporaryDirectory() as tmp_dir:
            fuchsia_dir = Path(tmp_dir)
            config_dir = fuchsia_dir / "build" / "bazel" / "config"
            config_dir.mkdir(parents=True)

            (config_dir / "main_workspace_top_dir").write_text(
                "gen/test/bazel_workspace\n"
            )

            (config_dir / "alt_workspace_top_dir").write_text(
                " alternative_workspace \n"
            )

            self.assertEqual(
                workspace_utils.get_bazel_relative_topdir(fuchsia_dir, "main"),
                "gen/test/bazel_workspace",
            )

            self.assertEqual(
                workspace_utils.get_bazel_relative_topdir(
                    str(fuchsia_dir), "main"
                ),
                "gen/test/bazel_workspace",
            )

            self.assertEqual(
                workspace_utils.get_bazel_relative_topdir(fuchsia_dir, "alt"),
                "alternative_workspace",
            )


class TestWorkspaceShouldExcludeFile(unittest.TestCase):
    def test_workspace_should_exclude_file(self):
        _EXPECTED_EXCLUDED_PATHS = [
            "out",
            ".jiri",
            ".fx",
            ".git",
            "bazel-bin",
            "bazel-repos",
            "bazel-out",
            "bazel-workspace",
        ]
        for path in _EXPECTED_EXCLUDED_PATHS:
            self.assertTrue(
                workspace_utils.workspace_should_exclude_file(path),
                msg=f"For path [{path}]",
            )

        _EXPECTED_INCLUDED_PATHS = [
            "out2",
            "src",
            ".clang-format",
            ".gn",
        ]
        for path in _EXPECTED_INCLUDED_PATHS:
            self.assertFalse(
                workspace_utils.workspace_should_exclude_file(path),
                msg=f"For path [{path}]",
            )


if __name__ == "__main__":
    unittest.main()
