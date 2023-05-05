# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import fidl_codec
import unittest


class FuchsiaControllerTest(unittest.TestCase):

    def setUp(self):
        fidl_codec.add_ir_path(
            "fidling/gen/src/developer/ffx/lib/fuchsia-controller/fidl/fuchsia.controller.test.fidl.json"
        )
