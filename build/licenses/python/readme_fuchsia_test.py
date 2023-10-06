#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tests for Readme and ReadmesDB."""


import dataclasses
from pathlib import Path
from typing import Dict
from file_access import FileAccess
from gn_label import GnLabel
from readme_fuchsia import Readme, ReadmesDB
import unittest


class ReadmeTest(unittest.TestCase):
    def setUp(self) -> None:
        return super().setUp()

    def test_readme_from_text(self):
        readme = Readme.from_text(
            readme_label=GnLabel.from_str("//some/path/README.fuchsia"),
            applicable_target=GnLabel.from_str("//some/other/path:to_target"),
            file_text="""
X: 123
Y: abc
Name: Some Name
License File: license1
License File: child/license2
License File: ../sibling/license3

Some text
""",
        )

        self.assertEqual(readme.package_name, "Some Name")
        self.assertEqual(
            readme.license_files,
            (
                GnLabel.from_str("//some/other/path/license1"),
                GnLabel.from_str("//some/other/path/child/license2"),
                GnLabel.from_str("//some/other/sibling/license3"),
            ),
        )


@dataclasses.dataclass
class MockFileAccess(FileAccess):
    readme_content_by_path: Dict[Path, str] = None

    def read_text(self, label: GnLabel) -> str:
        return self.readme_content_by_path[label.path]

    def file_exists(self, label: GnLabel) -> bool:
        return label.path in self.readme_content_by_path


class ReadmesDBTest(unittest.TestCase):
    db: ReadmesDB

    def setUp(self) -> None:
        readme_content_by_path = {
            Path(
                "foo/README.fuchsia"
            ): "Name: Never used - overrided via assets",
            Path("foo/bar/README.fuchsia"): "Name: Bar",
            Path(
                "vendor/google/tools/check-licenses/assets/readmes/foo/README.fuchsia"
            ): "Name: Foo",
            Path(
                "vendor/google/tools/check-licenses/assets/readmes/foo/baz/README.fuchsia"
            ): "Name: Baz",
            Path(
                "tools/check-licenses/assets/readmes/foo/qax/README.fuchsia"
            ): "Name: Qax",
        }

        file_access = MockFileAccess(
            fuchsia_source_path=None,
            readme_content_by_path=readme_content_by_path,
        )
        self.db = ReadmesDB(file_access=file_access)

        return super().setUp()

    def test_find_readme(self):
        readme = self.db.find_readme_for_label(GnLabel.from_str("//foo:x"))
        self.assertEqual(readme.package_name, "Foo")

        readme = self.db.find_readme_for_label(
            GnLabel.from_str("//something:x")
        )
        self.assertIsNone(readme)

        readme = self.db.find_readme_for_label(GnLabel.from_str("//foo/bar"))
        self.assertEqual(readme.package_name, "Bar")

        readme = self.db.find_readme_for_label(GnLabel.from_str("//foo/baz"))
        self.assertEqual(readme.package_name, "Baz")

        readme = self.db.find_readme_for_label(GnLabel.from_str("//foo/qax"))
        self.assertEqual(readme.package_name, "Qax")


if __name__ == "__main__":
    unittest.main()
