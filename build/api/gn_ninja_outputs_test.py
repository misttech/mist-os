#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, Path(__file__).parent)
import gn_ninja_outputs

_NINJA_OUTPUTS = {
    "//:foo": ["obj/foo.stamp"],
    "//:bar": [
        "obj/bar.output",
        "obj/bar.stamp",
    ],
    "//:zoo": [
        "obj/zoo.output",
        "obj/zoo.stamp",
    ],
    "//:zoo(//toolchain:secondary)": [
        "secondary/obj/zoo.output",
        "secondary/obj/zoo.stamp",
    ],
}


class TestNinjaOutputsDatabase(unittest.TestCase):
    def setUp(self):
        self._inputs_json_file = tempfile.NamedTemporaryFile(
            mode="wt", suffix=".json"
        )
        self.inputs_json = Path(self._inputs_json_file.name)
        json.dump(_NINJA_OUTPUTS, self._inputs_json_file)
        self._inputs_json_file.flush()

    def run_tests(self, db):
        self.assertEqual([], db.gn_label_to_paths("//:unknown"))
        for label, paths in _NINJA_OUTPUTS.items():
            self.assertListEqual(
                paths,
                db.gn_label_to_paths(label),
                f"When querying label {label}",
            )

        self.assertEqual("", db.path_to_gn_label("unknown_path"))
        for label, paths in _NINJA_OUTPUTS.items():
            for path in paths:
                self.assertEqual(
                    label,
                    db.path_to_gn_label(path),
                    f"When querying path {path}",
                )

        self.assertListEqual(sorted(_NINJA_OUTPUTS.keys()), db.get_labels())

        expected_paths = sorted(
            path for sublist in _NINJA_OUTPUTS.values() for path in sublist
        )
        self.assertListEqual(expected_paths, db.get_paths())

    def run_tests_for_class(self, db_class):
        db = db_class()
        db.load_from_json(self.inputs_json)
        self.run_tests(db)

        database_file = tempfile.NamedTemporaryFile(suffix=".database")
        database_path = Path(database_file.name)
        db.save_to_file(database_path)

        db = db_class()
        db.load_from_file(database_path)
        self.run_tests(db)

    def test_json_database(self):
        self.run_tests_for_class(gn_ninja_outputs.NinjaOutputsJSON)

    def test_tabular_database(self):
        self.run_tests_for_class(gn_ninja_outputs.NinjaOutputsTabular)


if __name__ == "__main__":
    unittest.main()
