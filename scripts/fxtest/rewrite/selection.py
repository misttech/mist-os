# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Implementation of test selection logic for `fx test`.

`fx test` supports fuzzy matching of tests across a number of
dimensions. This module implements selection and provides data
wrappers for the outcomes of the selection process.
"""

import asyncio
from collections import defaultdict
from enum import Enum
import re
import typing

import jellyfish

import event
import selection_types
from test_list_file import Test


class SelectionError(Exception):
    """There was an error preventing test selection from continuing."""


class SelectionMode(Enum):
    ANY = 0
    HOST = 1
    DEVICE = 2


# Default threshold for matching.
DEFAULT_FUZZY_DISTANCE_THRESHOLD: int = 3

# Large number to avoid matching a string.
NO_MATCH_DISTANCE: int = 1000000

# Perfect matches are distance 0.
PERFECT_MATCH_DISTANCE: int = 0


async def select_tests(
    entries: typing.List[Test],
    selection: typing.List[str],
    mode: SelectionMode = SelectionMode.ANY,
    fuzzy_distance_threshold: int = DEFAULT_FUZZY_DISTANCE_THRESHOLD,
    recorder: event.EventRecorder | None = None,
) -> selection_types.TestSelections:
    """Perform selection on the incoming list of tests.

    Selection may be passed directly from the command line. Each selection entry
    is implicitly ORed with adjacent entries unless a `--and` argument separates them.

    The --package and --component arguments each take a single argument.

    Args:
        entries (typing.List[Test]): Tests to select from.
        selection (typing.List[str]): Selection command line.
        mode (SelectionMode, optional): Selection mode. Defaults to ANY.
        fuzzy_distance_threshold (int, optional): Distance threshold for including tests in selection.
        recorder (EventRecorder, optional): If set, record match duration events.

    Raises:
        RuntimeError: _description_
        SelectionError: _description_

    Returns:
        TestSelections: Description of the selection process outcome.
    """

    filtered_entry_scores: typing.Dict[str, int] = {}
    if mode == SelectionMode.HOST:
        filtered_entry_scores = {
            test.info.name: NO_MATCH_DISTANCE
            for test in entries
            if not test.is_host_test()
        }
        entries = list(filter(Test.is_host_test, entries))
    elif mode == SelectionMode.DEVICE:
        filtered_entry_scores = {
            test.info.name: NO_MATCH_DISTANCE
            for test in entries
            if not test.is_device_test()
        }
        entries = list(filter(Test.is_device_test, entries))

    def make_final_scores(partial: typing.Dict[str, int]) -> typing.Dict[str, int]:
        filtered_entry_scores.update(partial)
        return filtered_entry_scores

    if not selection:
        # If no selection text is specified, select all tests and
        # report them all as perfect matches.
        return selection_types.TestSelections(
            entries.copy(),
            [],
            make_final_scores({test.info.name: 0 for test in entries}),
            [],
            fuzzy_distance_threshold,
        )

    match_groups = _parse_selection_command_line(selection)

    tests_to_run: typing.Set[Test] = set()
    group_matches: typing.List[
        typing.Tuple[selection_types.MatchGroup, typing.List[str]]
    ] = []
    best_matches: typing.Dict[str, int] = defaultdict(int)
    TRAILING_PATH = re.compile(r"/([\w\-_\.]+)$")
    COMPONENT_REGEX = re.compile(r"#meta/([\w\-_]+)\.cm")
    PACKAGE_REGEX = re.compile(r"/([\w\-_]+)#meta")

    def fast_match(s1: str, s2: str) -> int:
        """Perform a fast Damerau-Levenshtein Distance match on the given strings.

        Strings with significantly different lengths cannot possible be below
        the threshold, so to avoid expensive calculations this function
        returns NO_MATCH_DISTANCE for strings whose lengths differ by more than
        the current fuzzy distance threshold.

        Args:
            s1 (str): First string to match.
            s2 (str): Second string to match.

        Returns:
            int: Damerau-Levenshtein distance if strings are close
                in length, NO_MATCH_DISTANCE otherwise.
        """
        if abs(len(s1) - len(s2)) > fuzzy_distance_threshold:
            return NO_MATCH_DISTANCE
        return jellyfish.damerau_levenshtein_distance(s1, s2)

    def match_label(entry: Test, value: str) -> int:
        """Match build labels against a value.

        A build label starts with // and is followed by a path through the file system.

        For example "//src/sys" would match all tests defined under src/sys.

        Matchers return a perfect match (0) if value is a prefix, otherwise they return
        the result of fast_match (see definition.)

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            int: # of edits (including transposition) to match the
                strings. See fast_match.
        """
        if entry.build.test.label.strip("/").startswith(value.strip("/")):
            return PERFECT_MATCH_DISTANCE
        return fast_match(entry.build.test.label, value)

    def match_name(entry: Test, value: str) -> int:
        """Match test names against a value.

        A test name is a unique name for the test. It is typically a script path or component URL.

        For example "fuchsia-pkg://fuchsia.com/my-tests#meta/my-tests.cm" would match
        that exact test by name.

        Matchers return a perfect match (0) if value is a prefix, otherwise they return
        the result of fast_match (see definition.)

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            int: # of edits (including transposition) to match the
                strings. See fast_match.
        """
        if entry.info.name.startswith(value):
            return PERFECT_MATCH_DISTANCE
        return fast_match(entry.info.name, value)

    def match_component(entry: Test, value: str) -> int:
        """Match component names against a value.

        A component name is the part of the URL preceding ".cm".

        For example "my-component" is the component name for
        fuchsia-pkg://fuchsia.com/my-package#meta/my-component.cm

        Matchers return a perfect match (0) if value is a prefix, otherwise they return
        the result of fast_match (see definition.)

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            int: # of edits (including transposition) to match the
                strings. See fast_match.
        """
        if entry.build.test.package_url is None:
            return NO_MATCH_DISTANCE
        m = COMPONENT_REGEX.findall(entry.build.test.package_url)
        if m:
            if m[0].startswith(value):
                return PERFECT_MATCH_DISTANCE
            return fast_match(m[0], value)
        return NO_MATCH_DISTANCE

    def match_trailing_path(entry: Test, value: str) -> int:
        """Match the last element of the test path against a value.

        Host tests consist of a path to the binary to execute, and
        the last element of that path typically identifies the test.

        For example, "my_test_script" is the last element of the
        path for test "host_x64/tests/my_test_script".

        Matchers return a perfect match (0) if value is a prefix, otherwise they return
        the result of fast_match (see definition.)

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            int: # of edits (including transposition) to match the
                strings. See fast_match.
        """
        if entry.build.test.path is None:
            return NO_MATCH_DISTANCE
        m = TRAILING_PATH.findall(entry.build.test.path)
        if m:
            if m[0].startswith(value):
                return PERFECT_MATCH_DISTANCE
            return fast_match(m[0], value)
        return NO_MATCH_DISTANCE

    def match_package(entry: Test, value: str) -> int:
        """Match package names against a value.

        A package name is the last part of a URL path.

        For example "my-package" is the package name for
        fuchsia-pkg://fuchsia.com/my-package#meta/my-component.cm

        Matchers return a perfect match (0) if value is a prefix, otherwise they return
        the result of fast_match (see definition.)

        Args:
            entry (Test): Test to evaluate.
            value (str): Search string to match.

        Returns:
            int: # of edits (including transposition) to match the
                strings. See fast_match.
        """
        if entry.build.test.package_url is None:
            return NO_MATCH_DISTANCE
        m = PACKAGE_REGEX.findall(entry.build.test.package_url)
        if m:
            if m[0].startswith(value):
                return PERFECT_MATCH_DISTANCE
            return fast_match(m[0], value)
        return NO_MATCH_DISTANCE

    matchers = [
        match_label,
        match_name,
        match_component,
        match_package,
        match_trailing_path,
    ]

    for group in match_groups:
        id: event.Id | None = None
        if recorder is not None:
            id = recorder.emit_event_group(f"Matching {group}")
        matched: typing.List[str] = []

        def do_match():
            for entry in entries:
                # Calculate the worst matching {name, package name, component name}
                # for each value in the match group. Each matching
                # element has a score >= this value.
                name_worst = max(
                    [
                        min([matcher(entry, name) for matcher in matchers])
                        for name in group.names
                    ],
                    default=None,
                )
                package_worst = max(
                    [match_package(entry, name) for name in group.packages],
                    default=None,
                )
                component_worst = max(
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
                if final_score <= fuzzy_distance_threshold:
                    matched.append(entry.info.name)
                    tests_to_run.add(entry)
            group_matches.append((group, matched))

        await asyncio.to_thread(do_match)
        if recorder is not None and id is not None:
            recorder.emit_end(id=id)

    # Ensure tests match the input ordering for consistency.
    selected_tests = [e for e in entries if e in tests_to_run]

    return selection_types.TestSelections(
        selected_tests,
        [],
        make_final_scores(dict(best_matches)),
        group_matches,
        fuzzy_distance_threshold,
    )


def _parse_selection_command_line(
    selection: typing.List[str],
) -> typing.List[selection_types.MatchGroup]:
    selection = selection.copy()  # Do not affect input list.
    output_groups: typing.List[selection_types.MatchGroup] = []
    cur_group: selection_types.MatchGroup | None = None

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
        cur_group = selection_types.MatchGroup()

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
