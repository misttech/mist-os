# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import typing
import unittest

import args
import selection
import selection_types
import test_list_file
import tests_json_file


class MatchGroupTest(unittest.TestCase):
    """Test creating and printing MatchGroups."""

    def test_parse_empty(self):
        """Ensure that an empty selection produces no groups."""
        groups = selection._parse_selection_command_line([])
        self.assertEqual(len(groups), 0)

    def assertMatchContents(
        self,
        group: selection_types.MatchGroup,
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
        name_group = selection._parse_selection_command_line(["name"])
        package_group = selection._parse_selection_command_line(["--package", "name"])
        component_group = selection._parse_selection_command_line(
            ["--component", "name"]
        )

        self.assertMatchContents(name_group[0], {"name"}, set(), set())
        self.assertMatchContents(package_group[0], set(), {"name"}, set())
        self.assertMatchContents(component_group[0], set(), set(), {"name"})

        self.assertEqual(str(name_group[0]), "name")
        self.assertEqual(str(package_group[0]), "--package name")
        self.assertEqual(str(component_group[0]), "--component name")

    def test_parse_and(self):
        """Test logical AND parsing and formatting."""
        groups = selection._parse_selection_command_line(
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
        groups = selection._parse_selection_command_line(
            ["name", "--component", "component", "--package", "package"]
        )
        self.assertEqual(len(groups), 3)

        self.assertMatchContents(groups[0], {"name"}, set(), set())
        self.assertMatchContents(groups[1], set(), set(), {"component"})
        self.assertMatchContents(groups[2], set(), {"package"}, set())

    def test_all_together(self):
        """Test combination of values, ANDs, and ORs."""
        groups = selection._parse_selection_command_line(
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
            selection.SelectionError,
            lambda: selection._parse_selection_command_line(["--and", "value"]),
        )

        # Invalid to use --and after --and
        self.assertRaises(
            selection.SelectionError,
            lambda: selection._parse_selection_command_line(
                ["value", "--and", "--and"]
            ),
        )

        # Invalid to use --and at the end of a selection
        self.assertRaises(
            selection.SelectionError,
            lambda: selection._parse_selection_command_line(["value", "--and"]),
        )

        # --package and --component require an argument
        self.assertRaises(
            selection.SelectionError,
            lambda: selection._parse_selection_command_line(["value", "--component"]),
        )
        self.assertRaises(
            selection.SelectionError,
            lambda: selection._parse_selection_command_line(["value", "--package"]),
        )


class SelectTestsTest(unittest.IsolatedAsyncioTestCase):
    """Tests related to the selection of test entries to execute."""

    @staticmethod
    def _make_package_test(
        prefix: str, package: str, component: str
    ) -> test_list_file.Test:
        """Utility method to simulate a device Test

        Args:
            prefix (str): Prefix path for the label.
            package (str): Package name.
            component (str): Component name (without .cm)

        Returns:
            Test: Fake Test entry.
        """
        return test_list_file.Test(
            build=tests_json_file.TestEntry(
                test=tests_json_file.TestSection(
                    name=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                    label=f"//{prefix}:{package}($toolchain)",
                    os="fuchsia",
                    package_url=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                )
            ),
            info=test_list_file.TestListEntry(
                name=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                tags=[],
                execution=test_list_file.TestListExecutionEntry(
                    component_url=f"fuchsia-pkg://fuchsia.com/{package}#meta/{component}.cm",
                ),
            ),
        )

    @staticmethod
    def _make_host_test(prefix: str, name: str) -> test_list_file.Test:
        """Utility method to simulate a host Test

        Args:
            prefix (str): Prefix path for the label.
            name (str): Binary name, without host_x64.

        Returns:
            Test: Fake Test entry.
        """
        return test_list_file.Test(
            build=tests_json_file.TestEntry(
                test=tests_json_file.TestSection(
                    name=f"host_x64/{name}",
                    label=f"//{prefix}:{name}($toolchain)",
                    os="linux",
                    path=f"host_x64/{name}",
                )
            ),
            info=test_list_file.TestListEntry(
                name=f"host_x64/{name}",
                tags=[],
            ),
        )

    async def test_select_all(self):
        """Test that empty selection selects all mode-matching tests with perfect scores"""
        tests = [
            self._make_package_test("src/tests", "foo", "bar"),
            self._make_host_test("src/tests2", "baz"),
        ]

        selected = await selection.select_tests(tests, [])

        self.assertEqual(len(selected.selected), 2)
        for score in selected.best_score.values():
            self.assertEqual(score, selection.PERFECT_MATCH_DISTANCE)
        self.assertTrue(selected.has_device_test())

        host_selected = await selection.select_tests(
            tests, [], selection.SelectionMode.HOST
        )
        self.assertEqual(len(host_selected.selected), 1)
        self.assertEqual(
            host_selected.best_score["host_x64/baz"], selection.PERFECT_MATCH_DISTANCE
        )
        self.assertEqual(
            host_selected.best_score["fuchsia-pkg://fuchsia.com/foo#meta/bar.cm"],
            selection.NO_MATCH_DISTANCE,
        )
        self.assertFalse(host_selected.has_device_test())

        device_selected = await selection.select_tests(
            tests, [], selection.SelectionMode.DEVICE
        )
        self.assertEqual(len(device_selected.selected), 1)
        self.assertEqual(
            device_selected.best_score["fuchsia-pkg://fuchsia.com/foo#meta/bar.cm"],
            selection.PERFECT_MATCH_DISTANCE,
        )
        self.assertAlmostEqual(
            device_selected.best_score["host_x64/baz"], selection.NO_MATCH_DISTANCE
        )
        self.assertTrue(device_selected.has_device_test())

    async def test_prefix_matches(self):
        """Test that selecting prefixes of tests results in a perfect match."""

        tests = [
            self._make_package_test("src/tests", "foo-pkg", "bar-test"),
            self._make_host_test("src/other-tests", "binary_test"),
        ]
        select_path = await selection.select_tests(tests, ["//src/tests"])
        select_name1 = await selection.select_tests(tests, ["foo-pkg"])
        select_name2 = await selection.select_tests(tests, ["bar-test"])
        select_pkg = await selection.select_tests(tests, ["--package", "foo-pkg"])
        select_cm = await selection.select_tests(tests, ["--component", "bar-test"])
        url_prefix = await selection.select_tests(
            tests, ["fuchsia-pkg://fuchsia.com/foo-pkg"]
        )

        self.assertEqual(select_path.selected, select_name1.selected)
        self.assertEqual(select_path.selected, select_name2.selected)
        self.assertEqual(select_path.selected, select_pkg.selected)
        self.assertEqual(select_path.selected, select_cm.selected)
        self.assertEqual(select_path.selected, url_prefix.selected)

        for _, matches in select_path.group_matches:
            self.assertEqual(len(matches), 1)

        host_path = await selection.select_tests(tests, ["binary_test"])
        self.assertEqual(
            [s.info.name for s in host_path.selected], ["host_x64/binary_test"]
        )

        full_path = await selection.select_tests(tests, ["//src"])
        self.assertEqual(
            [s.info.name for s in full_path.selected], [t.info.name for t in tests]
        )

    async def test_approximate_matches(self):
        """Test that fuzzy matching catches common issues."""

        tests = [
            self._make_package_test("src/tests", "foo-pkg", "bar-test"),
            self._make_host_test("src/other-tests", "binary_test"),
        ]

        host_fuzzy = await selection.select_tests(tests, ["binaryytest"])
        self.assertEqual(
            [s.info.name for s in host_fuzzy.selected], ["host_x64/binary_test"]
        )
        self.assertEqual(host_fuzzy.best_score["host_x64/binary_test"], 1)
        self.assertEqual(host_fuzzy.fuzzy_distance_threshold, 3)

    async def test_flag_mutation(self):
        """Test that we can apply command line flag behavior to selections"""

        tests = [
            self._make_package_test("src/tests", "foo-pkg", "bar-test"),
            self._make_package_test("src/tests", "bar-pkg", "baz-test"),
            self._make_host_test("src/other-tests", "binary_test"),
            self._make_host_test("src/other-tests", "script_test"),
        ]

        select_all = await selection.select_tests(tests, [])
        select_all.apply_flags(args.parse_args(["--limit=3"]))
        self.assertEqual(len(select_all.selected), 3)
        self.assertEqual(len(select_all.selected_but_not_run), 1)
