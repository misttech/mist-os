#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for honeydew.affordances.fuchsia_controller.component.py."""

import unittest

from honeydew.affordances.fuchsia_controller import component as fc_component


# pylint: disable=protected-access
class ComponentFCTests(unittest.TestCase):
    """Unit tests for honeydew.affordances.fuchsia_controller.component.py."""

    def setUp(self) -> None:
        super().setUp()

        self.component_obj = fc_component.Component()

        self.assertIsInstance(self.component_obj, fc_component.Component)

    def test_launch(self) -> None:
        """Testcase for Component.launch()"""
        url = "fuchsia-pkg://fuchsia.com/sl4f#meta/sl4f.cmx"

        with self.assertRaises(NotImplementedError):
            self.component_obj.launch(url=url)

    def test_list(self) -> None:
        """Testcase for Component.list()"""
        with self.assertRaises(NotImplementedError):
            self.component_obj.list()

    def test_search(self) -> None:
        """Testcase for ComponentDefault.search()"""
        with self.assertRaises(NotImplementedError):
            self.component_obj.search(name="sl4f.cmx")


if __name__ == "__main__":
    unittest.main()
