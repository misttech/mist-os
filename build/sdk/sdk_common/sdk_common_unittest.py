#!/usr/bin/env fuchsia-vendored-python
# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# TODO(https://fxbug.dev/42058132): switch to the standard shebang line when the mocking library
# is available.

import unittest

from sdk_common import Atom, Validator, detect_category_violations


def _atom(name: str, category: str, area: str | None = None) -> Atom:
    return Atom(
        {
            "id": name,
            "meta": "somemeta.json",
            "category": category,
            "area": area,
            "gn-label": "//hello",
            "deps": [],
            "package-deps": [],
            "files": [],
            "tags": [],
            "type": "schema.json",
            "stable": True,
        }
    )


class SdkCommonTests(unittest.TestCase):
    def test_categories(self) -> None:
        atoms = [_atom("hello", "internal"), _atom("world", "partner")]
        self.assertEqual([*detect_category_violations("internal", atoms)], [])
        atoms = [
            _atom("hello", "compat_test"),
            _atom("world", "host_tool"),
            _atom("solar_system", "prebuilt"),
            _atom("universe", "partner"),
        ]
        self.assertEqual(
            [*detect_category_violations("compat_test", atoms)], []
        )

        atoms = [
            _atom("world", "host_tool"),
            _atom("solar_system", "prebuilt"),
            _atom("universe", "partner"),
        ]
        self.assertEqual(
            [*detect_category_violations("compat_test", atoms)], []
        )
        self.assertEqual([*detect_category_violations("host_tool", atoms)], [])

        atoms = [
            _atom("solar_system", "prebuilt"),
            _atom("universe", "partner"),
        ]
        self.assertEqual(
            [*detect_category_violations("compat_test", atoms)], []
        )
        self.assertEqual([*detect_category_violations("host_tool", atoms)], [])
        self.assertEqual([*detect_category_violations("prebuilt", atoms)], [])

        atoms = [_atom("universe", "partner")]
        self.assertEqual(
            [*detect_category_violations("compat_test", atoms)], []
        )
        self.assertEqual([*detect_category_violations("host_tool", atoms)], [])
        self.assertEqual([*detect_category_violations("prebuilt", atoms)], [])
        self.assertEqual([*detect_category_violations("partner", atoms)], [])

    def test_categories_failure(self) -> None:
        atoms = [_atom("hello", "compat_test"), _atom("world", "partner")]
        self.assertEqual(
            [*detect_category_violations("host_tool", atoms)],
            [
                '"hello" has publication level "compat_test", which is incompatible with "host_tool".'
            ],
        )
        self.assertEqual(
            [*detect_category_violations("prebuilt", atoms)],
            [
                '"hello" has publication level "compat_test", which is incompatible with "prebuilt".'
            ],
        )
        self.assertEqual(
            [*detect_category_violations("partner", atoms)],
            [
                '"hello" has publication level "compat_test", which is incompatible with "partner".'
            ],
        )

        atoms = [_atom("hello", "host_tool"), _atom("world", "partner")]
        self.assertEqual(
            [*detect_category_violations("prebuilt", atoms)],
            [
                '"hello" has publication level "host_tool", which is incompatible with "prebuilt".'
            ],
        )
        self.assertEqual(
            [*detect_category_violations("partner", atoms)],
            [
                '"hello" has publication level "host_tool", which is incompatible with "partner".'
            ],
        )

        atoms = [_atom("hello", "prebuilt"), _atom("world", "partner")]
        self.assertEqual(
            [*detect_category_violations("partner", atoms)],
            [
                '"hello" has publication level "prebuilt", which is incompatible with "partner".'
            ],
        )

    def test_category_name_unrecognized(self) -> None:
        atoms = [_atom("hello", "internal"), _atom("world", "public")]
        self.assertRaisesRegex(
            Exception,
            'Unknown SDK category "public"',
            lambda: [*detect_category_violations("internal", atoms)],
        )

    def test_area_name(self) -> None:
        v = Validator(valid_areas=["Kernel", "Unknown"])
        atoms = [
            _atom("hello", "internal", "Unknown"),
            _atom("world", "partner", "Kernel"),
        ]
        self.assertEqual([*v.detect_area_violations(atoms)], [])

        atoms = [
            _atom("hello", "internal", "So Not A Real Area"),
            _atom("world", "partner", "Kernel"),
        ]
        self.assertEqual(
            [*v.detect_area_violations(atoms)],
            [
                "hello specifies invalid API area 'So Not A Real Area'. Valid areas: ['Kernel', 'Unknown']"
            ],
        )

    def test_area_required(self) -> None:
        v = Validator(valid_areas=["Kernel", "Unknown"])
        atoms = [
            _atom("hello", "internal"),
            _atom("world", "partner", "Kernel"),
        ]
        self.assertEqual(
            [*v.detect_area_violations(atoms)],
            [
                "hello must specify an API area. Valid areas: ['Kernel', 'Unknown']",
            ],
        )

        atoms = [
            _atom("hello", "internal"),
            _atom("world", "partner"),
        ]
        self.assertEqual(
            [*v.detect_area_violations(atoms)],
            [
                "hello must specify an API area. Valid areas: ['Kernel', 'Unknown']",
                "world must specify an API area. Valid areas: ['Kernel', 'Unknown']",
            ],
        )

    def test_validator_detects_all_problems(self) -> None:
        v = Validator(valid_areas=["Unknown"])
        # Category violation
        atoms = [
            _atom("hello", "internal", "So Not A Real Area"),
            _atom("hello", "partner"),
        ]
        self.assertEqual(
            [*v.detect_violations("partner", atoms)],
            [
                """Targets sharing the SDK id hello:
 - //hello
 - //hello
""",
                '"hello" has publication level "internal", which is incompatible with "partner".',
                "hello specifies invalid API area 'So Not A Real Area'. Valid areas: ['Unknown']",
                "hello must specify an API area. Valid areas: ['Unknown']",
            ],
        )


if __name__ == "__main__":
    unittest.main()
