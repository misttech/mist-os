# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
import unittest
from sys import argv
from unittest import TestCase, main
from zipfile import Path, ZipFile

_doc_zip = argv.pop()


class Test(TestCase):
    _path: Path

    @classmethod
    def setUpClass(cls) -> None:
        cls._path = Path(_doc_zip)

    def testFileExists0(self) -> None:
        self.assertTrue(
            (self._path / "index.html").is_file(),
            msg=f"expected `index.html` to be a file in {repr(self._path)}",
        )

    def testFileContains1(self) -> None:
        found = (self._path / "index.html").read_text()
        self.assertIn(
            "List of all crates",
            found,
        )

    def testFileContains2(self) -> None:
        found = (self._path / "index.html").read_text()
        self.assertIn(
            "quebec",
            found,
        )

    def testFileExists3(self) -> None:
        self.assertTrue(
            (self._path / "quebec/struct.Quebec.html").is_file(),
            msg=f"expected `quebec/struct.Quebec.html` to be a file in {repr(self._path)}",
        )

    def testFileContainsRaw4(self) -> None:
        found = (self._path / "search-index.js").read_text()
        self.assertIn(
            "Quebec",
            found,
        )
