#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import filecmp
import json
import os
import shutil
import tempfile
import unittest
from unittest import mock

import update_platform_version

FAKE_VERSION_HISTORY_FILE_CONTENT = """{
    "data": {
        "name": "Platform version map",
        "type": "version_history",
        "api_levels": {
            "1" : {
                "abi_revision": "0x0000000000000001",
                "status": "in-development"
            }
        }
    },
    "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json"
}
"""

FAKE_FIDL_COMPATIBILITY_DOC_FILE_CONTENT = """
Some docs above.

{% set in_development_api_level = 1 %}

Some docs below.
"""

OLD_API_LEVEL = 1
OLD_SUPPORTED_API_LEVELS = [1]

NEW_API_LEVEL = 2
# This script doesn't update the set of supported API levels, this only happen
# when freezing an API level.
NEW_SUPPORTED_API_LEVELS = OLD_SUPPORTED_API_LEVELS

FAKE_SRC_FILE_CONTENT = "{ test }"
EXPECTED_DST_FILE_CONTENT = "{ test }"


class TestUpdatePlatformVersionMethods(unittest.TestCase):
    def setUp(self) -> None:
        self.test_dir = tempfile.mkdtemp()

        self.fake_version_history_file = os.path.join(
            self.test_dir, "version_history.json"
        )
        with open(self.fake_version_history_file, "w") as f:
            f.write(FAKE_VERSION_HISTORY_FILE_CONTENT)

        self.fake_fidl_compability_doc_file = os.path.join(
            self.test_dir, "fidl_api_compatibility_testing.md"
        )
        with open(self.fake_fidl_compability_doc_file, "w") as f:
            f.write(FAKE_FIDL_COMPATIBILITY_DOC_FILE_CONTENT)

        self.test_src_dir = tempfile.mkdtemp()
        self.fake_src_file = os.path.join(
            self.test_src_dir, "fuchsia_src.test.json"
        )
        with open(self.fake_src_file, "w") as f:
            f.write(FAKE_SRC_FILE_CONTENT)

        self.test_dst_dir = tempfile.mkdtemp()
        self.fake_dst_file = os.path.join(
            self.test_dst_dir, "fuchsia_dst.test.json"
        )

        self.test_dir = tempfile.mkdtemp()
        self.fake_golden_file = os.path.join(
            self.test_dir, "compatibility_testing_goldens.json"
        )

        content = [
            {
                "dst": self.fake_dst_file,
                "src": self.fake_src_file,
            },
        ]
        with open(self.fake_golden_file, "w") as f:
            json.dump(content, f)

    def tearDown(self) -> None:
        shutil.rmtree(self.test_dir)

    @mock.patch("secrets.randbelow")
    def test_update_version_history(self, mock_secrets: mock.MagicMock) -> None:
        mock_secrets.return_value = 0x1234ABC

        self.assertTrue(
            update_platform_version.update_version_history(
                NEW_API_LEVEL, self.fake_version_history_file
            )
        )

        with open(self.fake_version_history_file, "r") as f:
            updated = json.load(f)
            self.assertEqual(
                updated,
                {
                    "data": {
                        "name": "Platform version map",
                        "type": "version_history",
                        "api_levels": {
                            "1": {
                                "abi_revision": "0x0000000000000001",
                                "status": "supported",
                            },
                            "2": {
                                "abi_revision": "0x0000000001234ABC",
                                "status": "in-development",
                            },
                        },
                    },
                    "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json",
                },
            )

        self.assertFalse(
            update_platform_version.update_version_history(
                NEW_API_LEVEL, self.fake_version_history_file
            )
        )

    @mock.patch("secrets.randbelow")
    def test_abi_collision(self, mock_secrets: mock.MagicMock) -> None:
        # Return 0x1 the first time, and 0x4321 the second time. We expect the
        # second one to be used.
        mock_secrets.side_effect = [0x1, 0x4321CBA]

        self.assertTrue(
            update_platform_version.update_version_history(
                NEW_API_LEVEL, self.fake_version_history_file
            )
        )

        with open(self.fake_version_history_file, "r") as f:
            updated = json.load(f)
            self.assertEqual(
                updated,
                {
                    "data": {
                        "name": "Platform version map",
                        "type": "version_history",
                        "api_levels": {
                            "1": {
                                "abi_revision": "0x0000000000000001",
                                "status": "supported",
                            },
                            "2": {
                                "abi_revision": "0x0000000004321CBA",
                                "status": "in-development",
                            },
                        },
                    },
                    "schema_id": "https://fuchsia.dev/schema/version_history-22rnd667.json",
                },
            )

    def test_update_fidl_compatibility_doc(self) -> None:
        with open(self.fake_fidl_compability_doc_file) as f:
            lines = f.readlines()
        self.assertIn(
            f"{{% set in_development_api_level = {OLD_API_LEVEL} %}}\n", lines
        )
        self.assertNotIn(
            f"{{% set in_development_api_level = {NEW_API_LEVEL} %}}\n", lines
        )

        self.assertTrue(
            update_platform_version.update_fidl_compatibility_doc(
                NEW_API_LEVEL, self.fake_fidl_compability_doc_file
            )
        )

        with open(self.fake_fidl_compability_doc_file) as f:
            lines = f.readlines()
        self.assertNotIn(
            f"{{% set in_development_api_level = {OLD_API_LEVEL} %}}\n", lines
        )
        self.assertIn(
            f"{{% set in_development_api_level = {NEW_API_LEVEL} %}}\n", lines
        )

    def test_move_compatibility_test_goldens(self) -> None:
        self.assertTrue(
            update_platform_version.copy_compatibility_test_goldens(
                self.test_dir, NEW_API_LEVEL
            )
        )
        self.assertTrue(filecmp.cmp(self.fake_src_file, self.fake_dst_file))


if __name__ == "__main__":
    unittest.main()
