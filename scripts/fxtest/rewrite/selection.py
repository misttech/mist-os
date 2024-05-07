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
from dataclasses import dataclass
from enum import Enum
import itertools
import os
import re
import shutil
import tempfile
import typing

import event
import execution
import selection_types
from test_list_file import Test


class SelectionError(Exception):
    """There was an error preventing test selection from continuing."""


class SelectionMode(Enum):
    ANY = 0
    HOST = 1
    DEVICE = 2
    E2E = 3


# Default threshold for matching.
DEFAULT_FUZZY_DISTANCE_THRESHOLD: int = 3

# Large number to avoid matching a string.
NO_MATCH_DISTANCE: int = 1000000

# Perfect matches are distance 0.
PERFECT_MATCH_DISTANCE: int = 0


async def select_tests(
    entries: list[Test],
    selection: list[str],
    mode: SelectionMode = SelectionMode.ANY,
    fuzzy_distance_threshold: int = DEFAULT_FUZZY_DISTANCE_THRESHOLD,
    recorder: event.EventRecorder | None = None,
    exact_match: bool = False,
) -> selection_types.TestSelections:
    """Perform selection on the incoming list of tests.

    Selection may be passed directly from the command line. Each selection entry
    is implicitly ORed with adjacent entries unless a `--and` argument separates them.

    The --package and --component arguments each take a single argument.

    Args:
        entries (list[Test]): Tests to select from.
        selection (list[str]): Selection command line.
        mode (SelectionMode, optional): Selection mode. Defaults to ANY.
        fuzzy_distance_threshold (int, optional): Distance threshold for including tests in selection.
        recorder (EventRecorder, optional): If set, record match duration events.
        exact_match (bool, optional): If set, force exact matches only. Otherwise do fuzzy matching.

    Raises:
        RuntimeError: If the required dldist script is not found
        SelectionError: If the selections were invalid

    Returns:
        TestSelections: Description of the selection process outcome.
    """

    filtered_entry_scores: dict[str, int] = {}
    if mode == SelectionMode.HOST:
        filtered_entry_scores = {
            test.name(): NO_MATCH_DISTANCE
            for test in entries
            if not test.is_host_test()
        }
        entries = list(filter(Test.is_host_test, entries))
    elif mode == SelectionMode.DEVICE:
        filtered_entry_scores = {
            test.name(): NO_MATCH_DISTANCE
            for test in entries
            if not test.is_device_test()
        }
        entries = list(filter(Test.is_device_test, entries))
    elif mode == SelectionMode.E2E:
        filtered_entry_scores = {
            test.name(): NO_MATCH_DISTANCE
            for test in entries
            if not test.is_e2e_test()
        }
        entries = list(filter(Test.is_e2e_test, entries))

    def make_final_scores(partial: dict[str, int]) -> dict[str, int]:
        filtered_entry_scores.update(partial)
        return filtered_entry_scores

    if not selection:
        if exact_match:
            raise SelectionError(
                "A selection is required when --exact is specified"
            )
        # If no selection text is specified, select all tests and
        # report them all as perfect matches.
        return selection_types.TestSelections(
            entries.copy(),
            [],
            make_final_scores({test.name(): 0 for test in entries}),
            [],
            fuzzy_distance_threshold,
        )

    match_groups = _parse_selection_command_line(selection)

    tests_to_run: typing.Set[Test] = set()
    group_matches: list[tuple[selection_types.MatchGroup, list[str]]] = []
    best_matches: dict[str, int] = defaultdict(lambda: NO_MATCH_DISTANCE)
    TRAILING_PATH = re.compile(r"/([\w\-_\.]+)$")
    COMPONENT_REGEX = re.compile(r"#meta/([\w\-_]+)\.cm")

    def extract_label(entry: Test) -> str:
        return entry.build.test.label

    def extract_name(entry: Test) -> str:
        return entry.name()

    def extract_component(entry: Test) -> str | None:
        if entry.build.test.package_url is None:
            return None
        m = COMPONENT_REGEX.findall(entry.build.test.package_url)
        return m[0] if m else None

    def extract_trailing_path(entry: Test) -> str | None:
        if entry.build.test.path is None:
            return None
        m = TRAILING_PATH.findall(entry.build.test.path)
        return m[0] if m else None

    def extract_package(entry: Test) -> str | None:
        return entry.package_name()

    matched: list[str] = []
    match_tasks = []

    for group in match_groups:

        async def task_handler(group: selection_types.MatchGroup) -> None:
            id: event.Id | None = None
            if recorder is not None:
                # Hide the children of this event group. The runtime
                # and output of the programs run under the group are
                # useful in the log output, but too verbose to include
                # in the status output.
                id = recorder.emit_event_group(
                    f"Matching {group}", hide_children=True
                )

            label = _TestDistanceMeasurer(entries, extract_label)
            name = _TestDistanceMeasurer(entries, extract_name)
            component = _TestDistanceMeasurer(entries, extract_component)
            package = _TestDistanceMeasurer(entries, extract_package)
            trailing_path = _TestDistanceMeasurer(
                entries, extract_trailing_path
            )

            async def closest_name_match() -> list[_TestDistance]:
                lowest_score_dict: defaultdict[Test, int] = defaultdict(
                    lambda: NO_MATCH_DISTANCE
                )
                tests = []
                if not exact_match:
                    tasks = (
                        [label.distances(n, recorder, id) for n in group.names]
                        + [name.distances(n, recorder, id) for n in group.names]
                        + [
                            component.distances(n, recorder, id)
                            for n in group.names
                        ]
                        + [
                            package.distances(n, recorder, id)
                            for n in group.names
                        ]
                        + [
                            trailing_path.distances(n, recorder, id)
                            for n in group.names
                        ]
                    )
                else:
                    # In exact mode, don't match names against
                    # anything but the name field itself.
                    #
                    # For host tests, still allow exact matches against last
                    # segment of the name for more succinct selections.
                    tasks = [
                        name.distances(n, recorder, id, exact=True)
                        for n in group.names
                    ] + [
                        trailing_path.distances(n, recorder, id, exact=True)
                        for n in group.names
                    ]
                results: list[list[_TestDistance]] = await asyncio.gather(
                    *tasks
                )
                for result in itertools.chain(*results):
                    if result.test not in lowest_score_dict:
                        tests.append(result.test)
                    lowest_score_dict[result.test] = min(
                        lowest_score_dict[result.test], result.distance
                    )

                return [
                    _TestDistance(test, lowest_score_dict[test])
                    for test in tests
                ]

            match_tries = (
                [closest_name_match()]
                + [
                    component.distances(c, recorder, id)
                    for c in group.components
                ]
                + [package.distances(p, recorder, id) for p in group.packages]
            )

            distances: list[list[_TestDistance]] = await asyncio.gather(
                *match_tries
            )
            match_distances: defaultdict[Test, int] = defaultdict(
                lambda: NO_MATCH_DISTANCE
            )
            for td in itertools.chain(*distances):
                match_distances[td.test] = min(
                    match_distances[td.test], td.distance
                )

            for entry in entries:
                # The final score for a match group is the worst match
                # out of the above sets of scores.
                final_score = match_distances[entry]

                if exact_match and final_score != PERFECT_MATCH_DISTANCE:
                    # Allow only exact matches in exact match mode.
                    final_score = NO_MATCH_DISTANCE

                # Perform bookkeeping for debug output.
                best_matches[entry.name()] = min(
                    best_matches[entry.name()], final_score
                )

                # Record this test if it is now selected.
                if final_score <= fuzzy_distance_threshold:
                    matched.append(entry.name())
                    tests_to_run.add(entry)
            group_matches.append((group, matched))

            if recorder is not None and id is not None:
                recorder.emit_end(id=id)

        match_tasks.append(asyncio.create_task(task_handler(group)))

    await asyncio.wait(match_tasks, return_when=asyncio.FIRST_EXCEPTION)

    omitted_fuzzy_matches: list[Test] = []
    if PERFECT_MATCH_DISTANCE in best_matches.values():
        # There was a perfect match. Omit any test that is not a perfect match.
        omitted_fuzzy_matches = [
            t
            for t in tests_to_run
            if best_matches[t.name()] != PERFECT_MATCH_DISTANCE
        ]
        tests_to_run = {
            t
            for t in tests_to_run
            if best_matches[t.name()] == PERFECT_MATCH_DISTANCE
        }

    # Ensure tests match the input ordering for consistency.
    selected_tests = [e for e in entries if e in tests_to_run]

    return selection_types.TestSelections(
        selected_tests,
        omitted_fuzzy_matches,
        make_final_scores(dict(best_matches)),
        group_matches,
        fuzzy_distance_threshold,
    )


@dataclass
class _TestDistance:
    test: Test
    distance: int


class _TestDistanceMeasurer:
    def __init__(
        self,
        tests: list[Test],
        extractor: typing.Callable[[Test], str | None],
    ) -> None:
        self._test_and_key = [
            (test, y) for test in tests if (y := extractor(test)) is not None
        ]

    async def distances(
        self,
        value: str,
        recorder: event.EventRecorder | None,
        parent_id: event.Id | None,
        exact: bool = False,
    ) -> list[_TestDistance]:
        """Process and return the list of match distances for the list.

        Args:
            value (str): Value to compare against.
            recorder (event.EventRecorder | None): Recorder for events.
            parent_id (event.Id | None): ID of the event to nest under.
            exact (bool, optional): If true, force exact matches only. Defaults to False.

        Raises:
            RuntimeError: The required matching script is missing.

        Returns:
            list[_TestDistance]: Description of distances from contained set
            of tests to the input value.
        """
        with tempfile.TemporaryDirectory() as td:
            file_name = os.path.join(td, "temp-input.txt")
            with open(file_name, "w") as f:
                f.write("\n".join([v[1] for v in self._test_and_key]))
                f.flush()

            program_prefix = ["fx", "dldist"]
            if shutil.which("fx") is None:
                # Try to use dldist from the FUCHSIA_DIR, for test scenarios.
                expected_path = os.path.join(
                    os.getenv("FUCHSIA_DIR") or "", "bin", "dldist"
                )
                if os.path.exists(expected_path):
                    program_prefix = [expected_path]
                else:
                    # Find the directory containing this script, correcting for cases
                    # where it is run as a .pyz archive. Try to find dldist in the
                    # bin directory under that directory.
                    cur_path = os.path.dirname(__file__)
                    while not os.path.isdir(cur_path):
                        cur_path = os.path.split(cur_path)[0]
                    expected_path = os.path.join(cur_path, "bin", "dldist")
                    if os.path.exists(expected_path):
                        program_prefix = [expected_path]
                    else:
                        raise RuntimeError(
                            "Could not find matcher script dldist"
                        )

            arg_suffix = [
                "-v",
                "--needle",
                value,
                "--input",
                file_name,
            ] + (["--match-contains"] if not exact else [])

            output = await execution.run_command(
                *program_prefix,
                *arg_suffix,
                recorder=recorder,
                parent=parent_id,
            )

            if output is not None and output.return_code == 0:
                vals = [
                    int(line) for line in output.stdout.strip().splitlines()
                ]
                return [
                    _TestDistance(t[0], v)
                    for t, v in zip(self._test_and_key, vals)
                ]

            return []


def _parse_selection_command_line(
    selection: list[str],
) -> list[selection_types.MatchGroup]:
    selection = selection.copy()  # Do not affect input list.
    output_groups: list[selection_types.MatchGroup] = []
    cur_group: selection_types.MatchGroup | None = None

    def pop_for_arg(arg: str) -> None:
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

    def rotate_group() -> None:
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
                raise SelectionError(
                    "Cannot use --and at the beginning of a selection"
                )
            try:
                token = selection.pop(0)
                if token == "--and":
                    raise SelectionError(
                        "Cannot use --and immediately after --and"
                    )
                if token in special_tokens:
                    pop_for_arg(token)
                else:
                    cur_group.names.add(token)

            except IndexError:
                raise SelectionError(
                    "--and must be followed by another selection"
                )
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
