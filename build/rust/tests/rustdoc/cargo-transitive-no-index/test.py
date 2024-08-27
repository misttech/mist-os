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
        self.assertFalse(
            (self._path / "index.html").is_file(),
            msg=f"expected `index.html` to not be a file in {repr(self._path)}",
        )

    def testFileExists1(self) -> None:
        self.assertFalse(
            (self._path / "quebec/struct.Quebec.html").is_file(),
            msg=f"expected `quebec/struct.Quebec.html` to not be a file in {repr(self._path)}",
        )

    def testFileExists2(self) -> None:
        self.assertFalse(
            (self._path / "tango/trait.Tango.html").is_file(),
            msg=f"expected `tango/trait.Tango.html` to not be a file in {repr(self._path)}",
        )

    def testFileExists3(self) -> None:
        self.assertTrue(
            (self._path / "sierra/struct.Sierra.html").is_file(),
            msg=f"expected `sierra/struct.Sierra.html` to be a file in {repr(self._path)}",
        )

    def testFileContainsRaw4(self) -> None:
        found = (self._path / "sierra/struct.Sierra.html").read_text()
        self.assertIn(
            "Tango",
            found,
        )

    def testFileContainsRaw5(self) -> None:
        found = (self._path / "trait.impl/tango/trait.Tango.js").read_text()
        self.assertIn(
            "struct.Sierra.html",
            found,
        )

    def testFileContainsRaw6(self) -> None:
        found = (self._path / "search-index.js").read_text()
        self.assertNotIn(
            "Tango",
            found,
        )

    def testFileContainsRaw7(self) -> None:
        found = (self._path / "search-index.js").read_text()
        self.assertNotIn(
            "Quebec",
            found,
        )

    def testFileContainsRaw8(self) -> None:
        found = (self._path / "search-index.js").read_text()
        self.assertIn(
            "Sierra",
            found,
        )
