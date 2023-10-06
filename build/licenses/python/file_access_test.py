#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for FileAccess."""


from file_access import FileAccess
from gn_label import GnLabel
from pathlib import Path
import tempfile
import unittest


class FileAccessTest(unittest.TestCase):
    temp_dir: tempfile.TemporaryDirectory
    temp_dir_path: Path
    file_access: FileAccess

    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.temp_dir_path = Path(self.temp_dir.name)
        self.file_access = FileAccess(fuchsia_source_path=self.temp_dir_path)

        file_path1 = self.temp_dir_path / "foo"
        file_path1.write_text("FOO")

        file_path2 = self.temp_dir_path / "bar"
        file_path2.write_text("BAR")

        return super().setUp()

    def tearDown(self) -> None:
        self.temp_dir.cleanup()
        return super().tearDown()

    def test_file_exists(self):
        self.assertTrue(self.file_access.file_exists(GnLabel.from_str("//foo")))
        self.assertTrue(self.file_access.file_exists(GnLabel.from_str("//bar")))
        self.assertFalse(
            self.file_access.file_exists(GnLabel.from_str("//baz"))
        )

    def test_read_file(self):
        self.assertEqual(
            self.file_access.read_text(GnLabel.from_str("//foo")), "FOO"
        )
        self.assertEqual(
            self.file_access.read_text(GnLabel.from_str("//bar")), "BAR"
        )

    def test_depfile(self):
        self.file_access.read_text(GnLabel.from_str("//foo"))
        self.file_access.file_exists(GnLabel.from_str("//bar"))

        depfile_path = self.temp_dir_path / "depfile"
        self.file_access.write_depfile(
            dep_file_path=depfile_path, main_entry=Path("main")
        )

        actual_depfile_contents = depfile_path.read_text()
        expected_depfile_contents = f"""main:\\
    {self.temp_dir_path}/bar\\
    {self.temp_dir_path}/foo"""

        self.assertEqual(actual_depfile_contents, expected_depfile_contents)


if __name__ == "__main__":
    unittest.main()
