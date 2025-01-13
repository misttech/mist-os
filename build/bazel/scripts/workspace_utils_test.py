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


class TestForceSymlink(unittest.TestCase):
    def test_force_symlink(self):
        with tempfile.TemporaryDirectory() as tmp_dir:
            tmp_path = Path(tmp_dir).resolve()

            # Create a new symlink, then ensure its embedded target is relative.
            # The target doesn't need to exist.
            target_path = tmp_path / "target" / "file"
            link_path = tmp_path / "links" / "dir" / "symlink"

            workspace_utils.force_symlink(link_path, target_path)

            self.assertTrue(link_path.is_symlink())
            self.assertEqual(str(link_path.readlink()), "../../target/file")

            # Update the target to a new path, verify the symlink was updated.
            target_path = tmp_path / "target" / "new_file"

            workspace_utils.force_symlink(link_path, target_path)
            self.assertTrue(link_path.is_symlink())
            self.assertEqual(str(link_path.readlink()), "../../target/new_file")


class TestGeneratedWorkspaceFiles(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self.out = Path(self._td.name)
        (self.out / "elephant").write_text("trumpet!")

    def tearDown(self):
        self._td.cleanup()

    def test_with_no_file_hasher(self):
        ws_files = workspace_utils.GeneratedWorkspaceFiles()
        ws_files.record_file_content("zoo/lion", "roar!")
        ws_files.record_symlink("zoo/elephant", self.out / "elephant")
        ws_files.record_input_file_hash("no/such/file/exists")

        expected_json = r"""{
  "zoo/elephant": {
    "target": "@OUT@/elephant",
    "type": "symlink"
  },
  "zoo/lion": {
    "content": "roar!",
    "type": "file"
  }
}""".replace(
            "@OUT@", str(self.out)
        )

        self.assertEqual(ws_files.to_json(), expected_json)

        ws_files.write(self.out / "workspace")
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "lion").read_text(), "roar!"
        )
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "elephant").read_text(),
            "trumpet!",
        )
        self.assertEqual(
            str((self.out / "workspace" / "zoo" / "elephant").readlink()),
            "../../elephant",
        )

    def test_with_file_hasher(self):
        ws_files = workspace_utils.GeneratedWorkspaceFiles()
        ws_files.set_file_hasher(lambda path: f"SHA256[{path}]")
        ws_files.record_file_content("zoo/lion", "roar!")
        ws_files.record_symlink("zoo/elephant", self.out / "elephant")
        ws_files.record_input_file_hash("no/such/file/exists")

        expected_json = r"""{
  "no/such/file/exists": {
    "hash": "SHA256[no/such/file/exists]",
    "type": "input_file"
  },
  "zoo/elephant": {
    "target": "@OUT@/elephant",
    "type": "symlink"
  },
  "zoo/lion": {
    "content": "roar!",
    "type": "file"
  }
}""".replace(
            "@OUT@", str(self.out)
        )

        self.assertEqual(ws_files.to_json(), expected_json)

        ws_files.write(self.out / "workspace")
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "lion").read_text(), "roar!"
        )
        self.assertEqual(
            (self.out / "workspace" / "zoo" / "elephant").read_text(),
            "trumpet!",
        )
        self.assertEqual(
            str((self.out / "workspace" / "zoo" / "elephant").readlink()),
            "../../elephant",
        )
        self.assertFalse(
            (
                self.out / "workspace" / "no" / "such" / "file" / "exists"
            ).exists()
        )


if __name__ == "__main__":
    unittest.main()
