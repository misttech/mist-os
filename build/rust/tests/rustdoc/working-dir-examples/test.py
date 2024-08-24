# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
from sys import argv
from unittest import TestCase
from zipfile import Path

_doc_zip = argv.pop()


class Test(TestCase):
    _path: Path

    @classmethod
    def setUpClass(cls) -> None:
        cls._path = Path(_doc_zip)

    def testFileExists0(self) -> None:
        self.assertTrue(
            (self._path / "examples").is_file(),
            msg=f"expected `examples` to be a file in {repr(self._path)}",
        )
