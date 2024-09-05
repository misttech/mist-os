#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, Path(__file__).parent)
import build_api_filter


class BuildApiModulesFilterTest(unittest.TestCase):
    def _setup_build_dir(self, build_dir, api_files):
        api_list = sorted(api_files.keys())
        with (build_dir / "api.json").open("w") as f:
            json.dump(api_list, f, indent=2)

        for api_module, json_value in api_files.items():
            (build_dir / f"{api_file}.json").write_text(
                json.dumps(json_value, indent=2)
            )

    def test_filter(self):
        ninja_artifacts = ["obj/foo", "gen/bar"]
        module_filter = build_api_filter.BuildApiFilter(ninja_artifacts)

        _TEST_CASES = [
            {
                "archives": (
                    [
                        {
                            "name": "archive",
                            "path": "build-archive.tar",
                            "type": "tar",
                        },
                        {"name": "archive", "path": "gen/bar", "type": "tgz"},
                        {
                            "name": "archive",
                            "path": "build-archive.zip",
                            "type": "zip",
                        },
                    ],
                    [
                        {"name": "archive", "path": "gen/bar", "type": "tgz"},
                    ],
                ),
                "assembly_input_archives": (
                    [
                        {
                            "label": "//bundles/assembly:zircon.tgz(//build/toolchain/fuchsia:x64)",
                            "path": "obj/bundles/assembly/zircon.tgz",
                        },
                        {
                            "label": "//bundles/assembly:embeddable.tgz(//build/toolchain/fuchsia:x64)",
                            "path": "obj/foo",
                        },
                        {
                            "label": "//bundles/assembly:bootstrap.tgz(//build/toolchain/fuchsia:x64)",
                            "path": "obj/bundles/assembly/bootstrap.tgz",
                        },
                    ],
                    [
                        {
                            "label": "//bundles/assembly:embeddable.tgz(//build/toolchain/fuchsia:x64)",
                            "path": "obj/foo",
                        },
                    ],
                ),
            }
        ]

        for test_case in _TEST_CASES:
            for api_module, input_expected_pair in test_case.items():
                input_value, expected_value = input_expected_pair
                input_content = json.dumps(input_value, indent=2)
                expected_content = json.dumps(expected_value, indent=2)
                output_content = module_filter.filter_api_file(
                    api_module, input_content
                )
                self.assertEqual(output_content, expected_content)


if __name__ == "__main__":
    unittest.main()
