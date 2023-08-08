#!/usr/bin/env fuchsia-vendored-python
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

TEST_VERSION_HISTORY_FILE_CONTENT = {
    "data":
        {
            "name": "Platform version map",
            "type": "version_history",
            "api_levels":
                {
                    "1": {
                        "abi_revision": "0x1",
                        "status": "in-development"
                    }
                }
        },
    "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
}


class TestFreezePlatformVersionMethods(unittest.TestCase):

    def test_freeze_version_history(self):
        expected_version_history = {
            "data":
                {
                    "name": "Platform version map",
                    "type": "version_history",
                    "api_levels":
                        {
                            "1": {
                                "abi_revision": "0x1",
                                "status": "supported"
                            }
                        }
                },
            "schema_id":
                "https://fuchsia.dev/schema/version_history-22rnd667.json"
        }

        result = freeze_in_development_api_level.freeze_version_history(
            TEST_VERSION_HISTORY_FILE_CONTENT)
        self.assertEqual(result, expected_version_history)


if __name__ == "__main__":
    unittest.main()
