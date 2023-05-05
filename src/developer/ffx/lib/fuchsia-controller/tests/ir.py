# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from fidl_codec import add_ir_path
import unittest


class IR(unittest.TestCase):
    """Fuchsia Controller FIDL IR lookup tests"""

    def test_load_library_path_fails(self):
        with self.assertRaises(RuntimeError):
            add_ir_path("foihawoihfoiwhoiawhfiohwf")
