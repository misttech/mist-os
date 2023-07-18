# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Implementation of test selection logic for `fx test`.

`fx test` supports fuzzy matching of tests across a number of
dimensions. This module implements selection and provides data
wrappers for the outcomes of the selection process.

"""

from collections import defaultdict
from dataclasses import dataclass
from dataclasses import field
import re
import typing

import jellyfish

from test_list_file import Test


class SelectionError(Exception):
    """There was an error preventing test selection from continuing."""


@dataclass
class MatchGroup:
    """Description of one set of properties that must match a test for selection.

    All properties are logically ANDed together. Logical OR is represented by
    multiple MatchGroups applied in sequence.

    Examples:
      `--package foo --component bar`
        Matches tests with package foo OR component bar
      `--package foo --and --component bar`
        Matches only tests with package foo and component bar.
      `my-test --package foo --and unittest`
        Matches tests named "my-test" OR tests named "unittest" in package foo.
    """

    # Set of names to match. "Name" matches are the most permissive.
    names: typing.Set[str] = field(default_factory=set)

    # Set of package names to match. The package field must be a match.
    packages: typing.Set[str] = field(default_factory=set)

    # Set of component names to match. The component field must be a match.
    components: typing.Set[str] = field(default_factory=set)

    def __str__(self) -> str:
        """Create a human-readable representation of this match group.

        This aligns with the input flags to `fx test` such that a
        user can copy and paste the output of this function and
        produce an equivalent MatchGroup.

        Returns:
            str: The copy/pasteable command line representing this group.
        """
        elements = (
            list(self.names)
            + [f"--package {p}" for p in self.packages]
            + [f"--component {c}" for c in self.components]
        )
        return " --and ".join(elements)


# Global threshold for matching.
# TODO(b/291144505): Get this value from configuration.
THRESHOLD: float = 0.9


@dataclass
class TestSelections:
    """Return value for test selection.

    This class contains the list of selected tests as well as
    information related to the selection process for debugging.
    """

    # The list of tests selected, ordered by presence in tests.json.
    selected: typing.List[Test]

    # The best score calculated for each test in tests.json, including non-selected tests.
    best_score: typing.Dict[str, float]

    # List of match groups with the set of tests selected by that match group.
    group_matches: typing.List[typing.Tuple[MatchGroup, typing.List[str]]]

    # The threshold used to match these tests.
    threshold: float


def select_tests(
    entries: typing.List[Test], selection: typing.List[str]
) -> TestSelections:
    """Perform selection on the incoming list of tests.

    Selection may be passed directly from the command line. Each selection entry
    is implicitly ORed with adjacent entries unless a `--and` argument separates them.

    The --package and --component arguments each take a single argument.

    Args:
        entries (typing.List[Test]): Tests to select from.
        selection (typing.List[str]): Selection command line.

    Raises:
        RuntimeError: _description_
        SelectionError: _description_

    Returns:
        TestSelections: Description of the selection process outcome.
    """
    if not selection:
        # If no selection text is specified, select all tests and
        # report them all as perfect matches.
        return TestSelections(
            entries.copy(),
            {test.info.name: 1.0 for test in entries},
            [],
            THRESHOLD,
        )

    match_groups = _parse_selection_command_line(selection)

    tests_to_run: typing.Set[Test] = set()
    group_matches: typing.List[typing.Tuple[MatchGroup, typing.List[str]]] = []
    best_matches: typing.Dict[str, float] = defaultdict(float)
    TRAILING_PATH = re.compile(r"/([\w\-_\.]+)$")
    COMPONENT_REGEX = re.compile(r"#meta/([\w\-_]+)\.cm")
    PACKAGE_REGEX = re.compile(r"/([\w\-_]+)#meta")

    def match_label(entry: Test, value: str) -> float:
        """Match build labels against a value.

        A build label starts with // and is followed by a path through the file system.

        For example "//src/sys" would match all tests defined under src/sys.

        Matchers return a perfect match if value is a prefix, otherwise they return
        Jaro-Winkler similarity.

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            float: Score from 0 to 1, where 1 is a perfect match.
        """
        if entry.build.test.label.strip("/").startswith(value.strip("/")):
            return 1.0
        return jellyfish.jaro_winkler_similarity(entry.build.test.label, value)

    def match_name(entry: Test, value: str) -> float:
        """Match test names against a value.

        A test name is a unique name for the test. It is typically a script path or component URL.

        For example "fuchsia-pkg://fuchsia.com/my-tests#meta/my-tests.cm" would match
        that exact test by name.

        Matchers return a perfect match if value is a prefix, otherwise they return
        Jaro-Winkler similarity.

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            float: Score from 0 to 1, where 1 is a perfect match.
        """
        if entry.info.name.startswith(value):
            return 1.0
        return jellyfish.jaro_winkler_similarity(entry.info.name, value)

    def match_component(entry: Test, value: str) -> float:
        """Match component names against a value.

        A component name is the part of the URL preceding ".cm".

        For example "my-component" is the component name for
        fuchsia-pkg://fuchsia.com/my-package#meta/my-component.cm

        Matchers return a perfect match if value is a prefix, otherwise they return
        Jaro-Winkler similarity.

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            float: Score from 0 to 1, where 1 is a perfect match.
        """
        if not entry.build.test.package_url:
            return 0
        m = COMPONENT_REGEX.findall(entry.build.test.package_url)
        if m:
            if m[0].startswith(value):
                return 1.0
            return jellyfish.jaro_winkler_similarity(m[0], value)
        return 0

    def match_trailing_path(entry: Test, value: str) -> float:
        """Match the last element of the test path against a value.

        Host tests consist of a path to the binary to execute, and
        the last element of that path typically identifies the test.

        For example, "my_test_script" is the last element of the
        path for test "host_x64/tests/my_test_script".

        Matchers return a perfect match if value is a prefix, otherwise they return
        Jaro-Winkler similarity.

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            float: Score from 0 to 1, where 1 is a perfect match.
        """
        if entry.build.test.path is None:
            return 0
        m = TRAILING_PATH.findall(entry.build.test.path)
        if m:
            if m[0].startswith(value):
                return 1.0
            return jellyfish.jaro_winkler_similarity(m[0], value)
        return 0

    def match_package(entry: Test, value: str) -> float:
        """Match package names against a value.

        A package name is the last part of a URL path.

        For example "my-package" is the package name for
        fuchsia-pkg://fuchsia.com/my-package#meta/my-component.cm

        Matchers return a perfect match if value is a prefix, otherwise they return
        Jaro-Winkler similarity.

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            float: Score from 0 to 1, where 1 is a perfect match.
        """
        if not entry.build.test.package_url:
            return 0
        m = PACKAGE_REGEX.findall(entry.build.test.package_url)
        if m:
            if m[0].startswith(value):
                return 1.0
            return jellyfish.jaro_winkler_similarity(m[0], value)
        return 0

    matchers = [
        match_label,
        match_name,
        match_component,
        match_package,
        match_trailing_path,
    ]

    for group in match_groups:
        matched: typing.List[str] = []
        for entry in entries:
            # Calculate the worst matching {name, package name, component name}
            # for each value in the match group. Each matching
            # element has a score >= this value.
            name_worst = min(
                [
                    max([matcher(entry, name) for matcher in matchers])
                    for name in group.names
                ],
                default=None,
            )
            package_worst = min(
                [match_package(entry, name) for name in group.packages],
                default=None,
            )
            component_worst = min(
                [match_component(entry, name) for name in group.components],
                default=None,
            )

            # The final score for a match group is the worst match
            # out of the above sets of scores.
            final_score = min(
                [
                    x
                    for x in [name_worst, package_worst, component_worst]
                    if x is not None
                ]
            )

            # Perform bookkeeping for debug output.
            best_matches[entry.info.name] = max(
                best_matches[entry.info.name], final_score
            )

            # Record this test if it is now selected.
            if final_score >= THRESHOLD:
                matched.append(entry.info.name)
                tests_to_run.add(entry)
        group_matches.append((group, matched))

    # Ensure tests match the input ordering for consistency.
    selected_tests = [e for e in entries if e in tests_to_run]

    return TestSelections(selected_tests, dict(best_matches), group_matches, THRESHOLD)


def _parse_selection_command_line(
    selection: typing.List[str],
) -> typing.List[MatchGroup]:
    selection = selection.copy()  # Do not affect input list.
    output_groups: typing.List[MatchGroup] = []
    cur_group: typing.Optional[MatchGroup] = None

    def pop_for_arg(arg: str):
        """Mutate the outer cur_group variable depending on the contents of the argument.

        This closure handles parameters that take a value.

        Args:
            arg (str): Name of the argument expecting a value.

        Raises:
            RuntimeError: If an unknown argument was passed
            SelectionError: If a value is expected but we reached the end of the input.
        """
        nonlocal cur_group
        assert cur_group
        assert arg in ["--package", "--component"]
        try:
            token = selection.pop(0)
            if arg == "--package":
                cur_group.packages.add(token)
            elif arg == "--component":
                cur_group.components.add(token)
        except IndexError:
            raise SelectionError(f"Missing value for flag {arg}")

    def rotate_group():
        """Start populating a new MatchGroup. If the current group
        is not empty, keep track of it first.
        """
        nonlocal cur_group
        if cur_group:
            output_groups.append(cur_group)
        cur_group = MatchGroup()

    special_tokens = ["--package", "--component"]
    while selection:
        # Keep popping tokens, accounting for those that require another argument.
        token = selection.pop(0)
        if token == "--and":
            if not cur_group:
                raise SelectionError("Cannot use --and at the beginning of a selection")
            try:
                token = selection.pop(0)
                if token == "--and":
                    raise SelectionError("Cannot use --and immediately after --and")
                if token in special_tokens:
                    pop_for_arg(token)
                else:
                    cur_group.names.add(token)

            except IndexError:
                raise SelectionError("--and must be followed by another selection")
        elif token in special_tokens:
            rotate_group()
            pop_for_arg(token)
        else:
            rotate_group()
            assert cur_group is not None
            cur_group.names.add(token)

    # Final rotation to get the last MatchGroup added to the output.
    rotate_group()

    return output_groups
