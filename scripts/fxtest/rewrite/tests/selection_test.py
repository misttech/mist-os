# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import typing
import unittest

from selection import _parse_selection_command_line
from selection import MatchGroup
from selection import select_tests
from selection import SelectionError
from test_list_file import Test
from test_list_file import TestListEntry
from test_list_file import TestListExecutionEntry
from tests_json_file import TestEntry
from tests_json_file import TestSection


class MatchGroupTest(unittest.TestCase):
    """Test creating and printing MatchGroups."""

    def test_parse_empty(self):
        """Ensure that an empty selection produces no groups."""
        groups = _parse_selection_command_line([])
        self.assertEqual(len(groups), 0)

    def assertMatchContents(
        self,
        group: MatchGroup,
        names: typing.Set[str],
        packages: typing.Set[str],
        components: typing.Set[str],
    ):
        """Helper to assert string sets against MatchGroup fields.

        Args:
            group (MatchGroup): Group to check.
            names (typing.Set[str]): Expected names.
            packages (typing.Set[str]): Expected packages.
            components (typing.Set[str]): Expected components.
        """
        self.assertSetEqual(group.names, names)
        self.assertSetEqual(group.packages, packages)
        self.assertSetEqual(group.components, components)

    def test_parse_single(self):
        """Test parsing and formatting single arguments."""
        name_group = _parse_selection_command_line(["name"])
        package_group = _parse_selection_command_line(["--package", "name"])
        component_group = _parse_selection_command_line(["--component", "name"])

        self.assertMatchContents(name_group[0], {"name"}, set(), set())
        self.assertMatchContents(package_group[0], set(), {"name"}, set())
        self.assertMatchContents(component_group[0], set(), set(), {"name"})

        self.assertEqual(str(name_group[0]), "name")
        self.assertEqual(str(package_group[0]), "--package name")
        self.assertEqual(str(component_group[0]), "--component name")

    def test_parse_and(self):
        """Test logical AND parsing and formatting."""
        groups = _parse_selection_command_line(
            [
                "name",
                "--and",
                "--component",
                "component",
                "--and",
                "--package",
                "package",
            ]
        )
        self.assertEqual(len(groups), 1)

        self.assertMatchContents(groups[0], {"name"}, {"package"}, {"component"})
        self.assertEqual(
            str(groups[0]),
            "name --and --package package --and --component component",
        )

    def test_parse_or(self):
        """Test spreading arguments across multiple MatchGroups."""
        groups = _parse_selection_command_line(
            ["name", "--component", "component", "--package", "package"]
        )
        self.assertEqual(len(groups), 3)

        self.assertMatchContents(groups[0], {"name"}, set(), set())
        self.assertMatchContents(groups[1], set(), set(), {"component"})
        self.assertMatchContents(groups[2], set(), {"package"}, set())

    def test_all_together(self):
        """Test combination of values, ANDs, and ORs."""
        groups = _parse_selection_command_line(
            [
                "name",
                "--and",
                "name2",
                "--component",
                "component",
                "--and",
                "name3",
                "--package",
                "package",
                "--and",
                "name4",
            ]
        )
        self.assertEqual(len(groups), 3)

        self.assertMatchContents(groups[0], {"name", "name2"}, set(), set())
        self.assertMatchContents(groups[1], {"name3"}, set(), {"component"})
        self.assertMatchContents(groups[2], {"name4"}, {"package"}, set())

    def test_parse_errors(self):
        """Test various invalid scenarios."""

        # Invalid to start an expression with AND
        self.assertRaises(
            SelectionError, lambda: _parse_selection_command_line(["--and", "value"])
        )

        # Invalid to use --and after --and
        self.assertRaises(
            SelectionError,
            lambda: _parse_selection_command_line(["value", "--and", "--and"]),
        )

        # Invalid to use --and at the end of a selection
        self.assertRaises(
            SelectionError,
            lambda: _parse_selection_command_line(["value", "--and"]),
        )

        # --package and --component require an argument
        self.assertRaises(
            SelectionError,
            lambda: _parse_selection_command_line(["value", "--component"]),
        )
        self.assertRaises(
            SelectionError,
            lambda: _parse_selection_command_line(["value", "--package"]),
        )


class SelectTestsTest(unittest.TestCase):
    """Tests related to the selection of test entries to execute."""

    @staticmethod
    def _make_package_test(prefix: str, package: str, component: str) -> Test:
        """Utility method to simulate a device Test

        Args:
            prefix (str): Prefix path for the label.
            package (str): Package name.
            component (str): Component name (without .cm)

        Returns:
            Test: Fake Test entry.
        """
        return Test(
            build=TestEntry(
                test=TestSection(
                    name=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                    label=f"//{prefix}:{package}($toolchain)",
                    package_url=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                )
            ),
            info=TestListEntry(
                name=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                tags=[],
                execution=TestListExecutionEntry(
                    component_url=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                ),
            ),
        )

    @staticmethod
    def _make_host_test(prefix: str, name: str) -> Test:
        """Utility method to simulate a host Test

        Args:
            prefix (str): Prefix path for the label.
            name (str): Binary name, without host_x64.

        Returns:
            Test: Fake Test entry.
        """
        return Test(
            build=TestEntry(
                test=TestSection(
                    name=f"host_x64/{name}",
                    label=f"//{prefix}:{name}($toolchain)",
                    path=f"host_x64/{name}",
                )
            ),
            info=TestListEntry(
                name=f"host_x64/{name}",
                tags=[],
            ),
        )

    def test_select_all(self):
        """Test that empty selection selects all tests with perfect scores."""
        tests = [self._make_package_test("src/tests", "foo", "bar")]

        selection = select_tests(tests, [])

        self.assertEqual(len(selection.selected), 1)
        for score in selection.best_score.values():
            self.assertAlmostEqual(score, 1)

    def test_prefix_matches(self):
        """Test that selecting prefixes of tests results in a perfect match."""

        tests = [
            self._make_package_test("src/tests", "foo-pkg", "bar-test"),
            self._make_host_test("src/other-tests", "binary_test"),
        ]
        select_path = select_tests(tests, ["//src/tests"])
        select_name1 = select_tests(tests, ["foo-pkg"])
        select_name2 = select_tests(tests, ["bar-test"])
        select_pkg = select_tests(tests, ["--package", "foo-pkg"])
        select_cm = select_tests(tests, ["--component", "bar-test"])
        url_prefix = select_tests(tests, ["fuchsia-pkg://fuchsia.com/foo-pkg"])

        self.assertEqual(select_path.selected, select_name1.selected)
        self.assertEqual(select_path.selected, select_name2.selected)
        self.assertEqual(select_path.selected, select_pkg.selected)
        self.assertEqual(select_path.selected, select_cm.selected)
        self.assertEqual(select_path.selected, url_prefix.selected)

        for _, matches in select_path.group_matches:
            self.assertEqual(len(matches), 1)

        host_path = select_tests(tests, ["binary_test"])
        self.assertEqual(
            [s.info.name for s in host_path.selected], ["host_x64/binary_test"]
        )

        full_path = select_tests(tests, ["//src"])
        self.assertEqual(
            [s.info.name for s in full_path.selected], [t.info.name for t in tests]
        )

    def test_approximate_matches(self):
        """Test that fuzzy matching catches common issues."""

        tests = [
            self._make_package_test("src/tests", "foo-pkg", "bar-test"),
            self._make_host_test("src/other-tests", "binary_test"),
        ]

        host_fuzzy = select_tests(tests, ["binaryytest"])
        self.assertEqual(
            [s.info.name for s in host_fuzzy.selected], ["host_x64/binary_test"]
        )
        self.assertNotAlmostEqual(host_fuzzy.best_score["host_x64/binary_test"], 1)
