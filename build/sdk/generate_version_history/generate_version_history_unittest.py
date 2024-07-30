# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import datetime
import unittest

import generate_version_history


class GenerateVersionHistoryTests(unittest.TestCase):
    def test_replace_api_legacy(self) -> None:
        version_history = {
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {
                    "9": {
                        "abi_revision": "0xECDB841C251A8CB9",
                        "status": "unsupported",
                    },
                    "10": {
                        "abi_revision": "0xED74D73009C2B4E3",
                        "status": "supported",
                    },
                },
                "special_api_levels": {
                    "NEXT": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4291821568,
                        "status": "in-development",
                    },
                    "HEAD": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4292870144,
                        "status": "in-development",
                    },
                    "PLATFORM": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4293918720,
                        "status": "in-development",
                    },
                },
            },
        }
        generate_version_history.replace_special_abi_revisions_using_latest_numbered(
            version_history
        )

        self.assertEqual(
            version_history,
            {
                "data": {
                    "name": "Platform version map",
                    "type": "version_history",
                    "api_levels": {
                        "9": {
                            "abi_revision": "0xECDB841C251A8CB9",
                            "status": "unsupported",
                        },
                        "10": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "status": "supported",
                        },
                    },
                    "special_api_levels": {
                        "NEXT": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "as_u32": 4291821568,
                            "status": "in-development",
                        },
                        "HEAD": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "as_u32": 4292870144,
                            "status": "in-development",
                        },
                        "PLATFORM": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "as_u32": 4293918720,
                            "status": "in-development",
                        },
                    },
                },
            },
        )

    def test_replace_api_new(self) -> None:
        version_history = {
            "data": {
                "name": "Platform version map",
                "type": "version_history",
                "api_levels": {
                    "9": {
                        "abi_revision": "0xECDB841C251A8CB9",
                        "status": "unsupported",
                    },
                    "10": {
                        "abi_revision": "0xED74D73009C2B4E3",
                        "status": "supported",
                    },
                },
                "special_api_levels": {
                    "NEXT": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4291821568,
                        "status": "in-development",
                    },
                    "HEAD": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4292870144,
                        "status": "in-development",
                    },
                    "PLATFORM": {
                        "abi_revision": "GENERATED_BY_BUILD",
                        "as_u32": 4293918720,
                        "status": "in-development",
                    },
                },
            },
        }
        generate_version_history.replace_special_abi_revisions_using_commit_hash_and_date(
            version_history,
            "e821407c0ffd857326038a504d76c5e11b738608",
            datetime.datetime(2024, 7, 19, 18, 59, 39, 0, datetime.UTC),
        )

        self.assertEqual(
            version_history,
            {
                "data": {
                    "name": "Platform version map",
                    "type": "version_history",
                    "api_levels": {
                        "9": {
                            "abi_revision": "0xECDB841C251A8CB9",
                            "status": "unsupported",
                        },
                        "10": {
                            "abi_revision": "0xED74D73009C2B4E3",
                            "status": "supported",
                        },
                    },
                    "special_api_levels": {
                        "NEXT": {
                            "abi_revision": "0xFF00E821000B470F",
                            "as_u32": 4291821568,
                            "status": "in-development",
                        },
                        "HEAD": {
                            "abi_revision": "0xFF00E821000B470F",
                            "as_u32": 4292870144,
                            "status": "in-development",
                        },
                        "PLATFORM": {
                            "abi_revision": "0xFF01E821000B470F",
                            "as_u32": 4293918720,
                            "status": "in-development",
                        },
                    },
                },
            },
        )

        # Double check that the day ordinal from the ABI stamp makes sense.
        self.assertEqual(
            datetime.date.fromordinal(0x000B470F), datetime.date(2024, 7, 20)
        )


if __name__ == "__main__":
    unittest.main()
