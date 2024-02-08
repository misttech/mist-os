#!/usr/bin/env fuchsia-vendored-python
# Copyright 2018 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# TODO(https://fxbug.dev/42058132): switch to the standard shebang line when the mocking library
# is available.

import unittest
from sdk_common import Atom, Validator, detect_category_violations


def _atom(name: str, category: str, area="Unknown") -> Atom:
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
        }
    )


class SdkCommonTests(unittest.TestCase):
    def test_categories(self) -> None:
        atoms = [_atom("hello", "internal"), _atom("world", "public")]
        self.assertEqual([*detect_category_violations("internal", atoms)], [])
        atoms = [_atom("hello", "internal"), _atom("world", "cts")]
        self.assertEqual([*detect_category_violations("internal", atoms)], [])

    def test_categories_failure(self) -> None:
        atoms = [_atom("hello", "internal"), _atom("world", "public")]
        self.assertEqual(
            [*detect_category_violations("partner", atoms)],
            ["hello has publication level internal, incompatible with partner"],
        )
        atoms = [_atom("hello", "internal"), _atom("world", "public")]
        self.assertEqual(
            [*detect_category_violations("cts", atoms)],
            ["hello has publication level internal, incompatible with cts"],
        )

    def test_category_name_bogus(self) -> None:
        atoms = [_atom("hello", "foobarnotgood"), _atom("world", "public")]
        self.assertRaises(
            Exception, lambda: [*detect_category_violations("partner", atoms)]
        )

    def test_area_name(self) -> None:
        v = Validator(valid_areas=["Kernel", "Unknown"])
        atoms = [
            _atom("hello", "internal", "Unknown"),
            _atom("world", "public", "Kernel"),
        ]
        self.assertEqual([*v.detect_area_violations(atoms)], [])

        atoms = [
            _atom("hello", "internal", "So Not A Real Area"),
            _atom("world", "public", "Kernel"),
        ]
        self.assertEqual(
            [*v.detect_area_violations(atoms)],
            [
                "hello specifies invalid API area 'So Not A Real Area'. Valid areas: ['Kernel', 'Unknown']"
            ],
        )

    def test_validator_detects_all_problems(self) -> None:
        v = Validator(valid_areas=["Unknown"])
        # Category violation
        atoms = [
            _atom("hello", "internal", "So Not A Real Area"),
            _atom("hello", "public", "Unknown"),
        ]
        self.assertEqual(
            [*v.detect_violations("partner", atoms)],
            [
                """Targets sharing the SDK id hello:
 - //hello
 - //hello
""",
                "hello has publication level internal, incompatible with partner",
                "hello specifies invalid API area 'So Not A Real Area'. Valid areas: ['Unknown']",
            ],
        )


if __name__ == "__main__":
    unittest.main()
