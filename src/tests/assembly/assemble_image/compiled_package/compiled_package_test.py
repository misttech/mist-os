#!/usr/bin/env python3.8
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import sys
import os
import json

assembly_outdir = sys.argv.pop()


class CompiledPackageTest(unittest.TestCase):
    """
    Validate the assembly outputs when using a compiled package
    """

    def test_assembly_has_core_package(self):
        outdir = os.path.join(assembly_outdir, "outdir")
        manifest = json.load(open(os.path.join(outdir, "image_assembly.json")))
        self.assertIn(
            os.path.join(outdir, "core",
                         "package_manifest.json"), manifest["base"],
            "The image assembly config should have 'core' in the base set")


if __name__ == '__main__':
    unittest.main()
