# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

import fidl_codec


class FuchsiaControllerTest(unittest.TestCase):
    def setUp(self) -> None:
        fidl_codec.add_ir_path(  # type: ignore[attr-defined] # we don't have types for this cpp file
            "fidling/gen/src/developer/ffx/lib/fuchsia-controller/fidl/fuchsia.controller.test.fidl.json"
        )
        fidl_codec.add_ir_path(  # type: ignore[attr-defined] # we don't have types for this cpp file
            "fidling/gen/src/developer/ffx/lib/fuchsia-controller/fidl/fuchsia.controller.othertest.fidl.json"
        )
