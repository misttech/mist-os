#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import shutil
import tempfile
import unittest

import freeze_in_development_api_level

SUPPORTED_API_LEVELS = [1]

IN_DEVELOPMENT_API_LEVEL = 2

class TestFreezePlatformVersionMethods(unittest.TestCase):

    def setUp(self):
        self.test_dir = tempfile.mkdtemp()

        self.fake_milestone_version_file = os.path.join(
            self.test_dir, 'platform_version.json')
        with open(self.fake_milestone_version_file, 'w') as f:
            pv = {
                'in_development_api_level': IN_DEVELOPMENT_API_LEVEL,
                'supported_fuchsia_api_levels': SUPPORTED_API_LEVELS,
            }
            json.dump(pv, f)
        freeze_in_development_api_level.PLATFORM_VERSION_PATH = self.fake_milestone_version_file


    def tearDown(self):
        shutil.rmtree(self.test_dir)


    def _api_level_is_frozen(self, api_level):
        with open(PLATFORM_VERSION_PATH, "r+") as f:
            platform_version = json.load(f)
            return api_level in platform_version["supported_fuchsia_api_levels"]


    def test_freeze_version_history(self):
        self.assertFalse(_api_level_is_frozen(NEW_API_LEVEL))

        freeze_in_development_api_level.freeze_in_development_api_level()

        self.assertTrue(_api_level_is_frozen(NEW_API_LEVEL))


if __name__ == '__main__':
    unittest.main()