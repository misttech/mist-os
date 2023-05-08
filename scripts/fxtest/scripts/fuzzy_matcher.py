#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
from collections import defaultdict
import colorama
from colorama import Fore, Style
import jellyfish
import json
import os
import re
import sys
import time
import typing
import dataclasses

colorama.init(strip=bool(os.getenv("NO_COLOR", None)))

DEFAULT_THRESHOLD = 0.75


def command(args: argparse.Namespace):
    """Fuzzy match build targets and tests.

    Example: fx search-tests my-component
             (Finds all test rules similar to "my-component")
    Example: fx search-tests my-component --threshold 0.5
             (Same as the above, with a lowered threshold for matching)
    Example: fx search-tests my-component --no-color
             (...without ANSI colors)
    Example: fx search-tests my-component --max-results 15
             (...with at most 15 results)
    Example: fx search-tests my-component --omit-test-file
             (...without results appearing the in the tests.json file)
    Example: fx search-tests my-component --debug
             (...with verbose debug timing information)
    """
    if args.threshold < 0 or args.threshold > 1:
        raise Exception("--threshold must be between 0 and 1")

    with TimingTracker("Create search locations"):
        search_locations: SearchLocations = create_search_locations()
    with TimingTracker("Create test file matcher"):
        tests_file_matcher: TestsFileMatcher = TestsFileMatcher(
            search_locations.tests_json_file
        )
    with TimingTracker("Create build file matcher"):
        build_file_matcher: BuildFileMatcher = BuildFileMatcher(
            search_locations.fuchsia_directory
        )

    matcher = Matcher(args.threshold)

    with TimingTracker("Find matches in tests file"):
        tests_matches = tests_file_matcher.find_matches(args.match_string, matcher)
    with TimingTracker("Find matches in build files"):
        build_matches = build_file_matcher.find_matches(args.match_string, matcher)

    test_match_names = set([suggestion.matched_name for suggestion in tests_matches])

    # Create a list of all matches. If the same value appeared in both a
    # BUILD.gn file and the tests.json file, prefer the tests.json version.
    # This is to avoid suggesting adding build targets that are already
    # included.
    #
    # Even if we omit results from the tests.json file, we still want to avoid
    # listing them as BUILD.gn results, since fx test presumably already
    # included them as its own suggestions.
    all_matches = list(tests_matches) if not args.omit_test_file else []
    all_matches += [
        match for match in build_matches if match.matched_name not in test_match_names
    ]
    all_matches.sort(key=lambda x: x.similarity, reverse=True)

    if not all_matches:
        print("No matching tests could be found in your Fuchsia checkout.")
        return

    for suggestion in all_matches[: args.max_results]:
        styled_name = color_output(
            [Fore.WHITE, Style.BRIGHT], suggestion.matched_name, not args.no_color
        )
        similarity_color = Fore.GREEN if suggestion.similarity > 0.85 else Fore.YELLOW
        styled_similarity = color_output(
            [similarity_color, Style.BRIGHT],
            f"{100*suggestion.similarity:.2f}%",
            not args.no_color,
        )
        styled_suggestion = color_output(
            [Style.DIM], suggestion.full_suggestion, not args.no_color
        )
        print(
            f"{styled_name} ({styled_similarity} similar)" f"\n    {styled_suggestion}"
        )

    if len(all_matches) > args.max_results:
        remaining_matches_line = (
            f"({len(all_matches) - args.max_results} more matches not shown)"
        )
        print(f"{color_output(Style.DIM, remaining_matches_line, not args.no_color)}")

    if args.debug:
        TimingTracker.print_timings()

    return


def color_output(
    styles: typing.Union[str, typing.List[str]], content: str, enabled: bool
) -> str:
    """Utility function to wrap a string with color styles.

    Args:
        styles: One or more styles defined by colorama.
        content: The string to style.
        enabled: If false, do not actually do styling. Used to switch by config.

    Returns:
        Styled output as a string.
    """
    if not isinstance(styles, list):
        styles = [styles]

    if not enabled:
        return content

    output = ""
    for style in styles:
        output += style
    output += content
    output += colorama.Style.RESET_ALL
    return output


class TimingTracker:
    """Utility class for tracking timings of different operations.

    This class is a ContextManager that tracks timing for wrapper operations.
    """

    _tracked_timing: typing.List[typing.Tuple[str, typing.Union[float, None]]] = []

    def __init__(self, operation_name: str) -> None:
        """Initialize a timing tracker instance.

        Args:
            operation_name: The name of the operation being tracked.
        """
        self._operation_name: str = operation_name
        self._slot: int | None = None
        self._start: float | None = None

    def __enter__(self):
        self._slot = len(TimingTracker._tracked_timing)
        self._start = time.monotonic()
        TimingTracker._tracked_timing.append((self._operation_name, None))

    def __exit__(self, *args):
        TimingTracker._tracked_timing[self._slot] = (
            self._operation_name,
            time.monotonic() - self._start,
        )

    @staticmethod
    def reset():
        """Reset all globally recorded timings."""
        TimingTracker._tracked_timing = []

    @staticmethod
    def print_timings():
        """Print all accumulated timings to stdout."""
        print("Debug timings:")
        for name, maybe_duration in TimingTracker._tracked_timing:
            if not isinstance(maybe_duration, float):
                continue
            print(f"  {name:40s}  {1000*maybe_duration:9.3f}ms")


class Matcher:
    """Implements the matching algorithm for two strings.

    The matching is subject to a configurable threshold, and matches
    with a similarity score below the threshold returns None.

    Returning None for scores below the threshold allows the threshold
    to be defined up front and used for the duration of execution.

    Attributes:
        threshold: The threshold for a match in the range [0,1]
    """

    def __init__(self, threshold: float):
        self.threshold: float = threshold

    def match(self, s1: str, s2: str) -> typing.Optional[float]:
        """Calculate the similarity of two strings

        Args:
            s1: The first string to match.
            s2: The second string to match.

        Returns:
            Similarity metric between 0 and 1 if >= the threshold. Otherwise
            None.

            >>> Matcher(threshold=1).match('abcd', 'abcd')
            1
            >>> Matcher(threshold=1).match('abcd', 'efgh')
            None
            >>> Matcher(threshold=0).match('abcd', 'efgh')
        """

        # Do some light normalization. Specifically make everything
        # lowercase and normalize all [-_] to just _.

        s1 = s1.lower().replace("-", "_")
        s2 = s2.lower().replace("-", "_")

        similarity = jellyfish.jaro_winkler_similarity(s1, s2)
        if similarity >= self.threshold:
            return similarity
        else:
            return None


@dataclasses.dataclass
class Suggestion:
    """A single suggestion returned from the matcher.

    Attributes:
        matched_name: The (short) name that was matched against.
        similarity: Measure of similarity to search term. 0 = no match, 1 = perfect match
        full_suggestion: A longer string describing this match to the user.
    """

    matched_name: str
    similarity: float
    full_suggestion: str

    def __repr__(self):
        return f"Suggestion({self.matched_name}, {self.similarity}, {self.full_suggestion})"


@dataclasses.dataclass
class SearchLocations:
    """Wrapper for search locations parsed from the environment.

    Attributes:
        fuchsia_directory: The root of the Fuchsia source tree.
        tests_json_file: Path to tests.json, listing tests for this build.
    """

    fuchsia_directory: str
    tests_json_file: str

    def __repr__(self) -> str:
        return str(self.__dict__)


class BuildFileMatcher:
    """Given a source directory, support searching for matching targets."""

    def __init__(self, source_directory: str):
        # We do not want to recurse into these directories, because
        # they contain a lot of files and no relevant BUILD.gn files
        # containing tests.
        IGNORED_DIRECTORIES = ["out", "third_party", "prebuilt"]

        build_file_paths = []
        for top_dir in os.listdir(source_directory):
            if top_dir in IGNORED_DIRECTORIES:
                continue

            if top_dir[:1] == ".":
                # Skip dotfiles and directories.
                continue

            if not os.path.isdir(os.path.join(source_directory, top_dir)):
                # Skip non-directories.
                continue

            with TimingTracker(f"..Walking directory {top_dir}"):
                for root, _, files in os.walk(os.path.join(source_directory, top_dir)):
                    for file in files:
                        if file == "BUILD.gn":
                            build_file_paths.append(os.path.join(root, file))

        self._package_finder = re.compile(r"test_package\(\"([^\"]+)\"\)")
        self._component_finder = re.compile(r"test_component\(\"([^\"]+)\"\)")
        self._package_name_finder = re.compile(r"package_name\s*=\s*\"([^\"]+)\"")
        self._component_name_finder = re.compile(r"component_name\s*=\s*\"([^\"]+)\"")

        parse_results: typing.List[typing.Dict[str, typing.List[str]]] = []
        with TimingTracker("..Parse BUILD.gn files"):
            parse_results = list(
                map(
                    lambda path: self._parse_build_file(path, source_directory),
                    build_file_paths,
                )
            )

        with TimingTracker("..Collect results"):
            name_to_target: defaultdict[str, typing.List[str]] = defaultdict(list)
            for result in parse_results:
                for key, value in result.items():
                    name_to_target[key] += value
            self._name_to_target: typing.Dict[str, typing.List[str]] = dict(
                name_to_target
            )

    def _parse_build_file(
        self, build_file: str, source_directory: str
    ) -> typing.Dict[str, typing.List[str]]:
        """Parse a build file into a mapping from name to referencing build targets.

        The output of this method is used to provide a set of names
        to search over along with suggestions for BUILD.gn rules
        that will include that named test in a local build.

        The algorithm in this method is approximate, and makes the following
        assumptions based on typical test target definitions in the Fuchsia
        source tree:
        - Test package rules end in "_test_package"
        - Test component rules end in "_test_component"
        - Test packages may have their name overridden by "package_name"
        - Test packages may contain "test_components". If one does,
        those component names are associated with the package target.
        If it does not, the component name is the same as the package
        name.
        - If a test package has a "component_name" that will also
        be used to override "package_name".

        As an example, consider:
        fuchsia_component("my_component") {
            ...
        }
        fuchsia_test_package("my_test_package") {
            test_components = [":my_component"]
            ...
        }
        fuchsia_unittest_package("my_unittest") {
            ...
        }

        The output for the above file at src/BUILD.gn would be:
        {
            "my_component": ["//src:my_test_package"],
            "my_test_package": ["//src:my_test_package"],
            "my_unittest": ["//src:my_unittest"]
        }

        Arguments:
            build_file: Path to a BUILD.gn file, relative to the source directory.
            source_directory: Relative path to the source directory.

        Returns:
            A mapping from test names to the BUILD.gn target for that test.
        """
        name_to_target = defaultdict(list)
        with open(build_file, "r") as f:
            build_rule_prefix = "//" + os.path.relpath(
                os.path.dirname(build_file), source_directory
            )

            contents = f.read()

            # Iterate over test components in the file, and keep track
            # of what we find in a list of (target, name) pairs.
            components_in_file: typing.List[typing.Tuple[str, str]] = []
            component_finds = re.finditer(self._component_finder, contents)
            for find in component_finds:
                component_target_name = find.group(1)
                component_name = find.group(1)

                # Extract any component_name overrides in the block.
                # Searching for a closing '}' without maintaining
                # context is error prone, but for our purposes
                # of identifying overrides (which typically appear
                # near the top of the build rule) it gives us the
                # desired output in all known cases.
                startpos = find.end()
                endpos = contents[startpos:].find("}") + startpos
                maybe_component_name_override = re.findall(
                    self._component_name_finder, contents[startpos:endpos]
                )

                # Every component has a name. By default it is the name of
                # the target itself, but it may be overridden using the
                # component_name parameter.
                if maybe_component_name_override:
                    component_name = maybe_component_name_override[0]

                components_in_file.append(
                    (
                        component_target_name,
                        component_name,
                    )
                )

            # Iterate over all of the test packages in the file.
            package_finds = list(re.finditer(self._package_finder, contents))
            for find in package_finds:
                package_name = find.group(1)
                target_name = find.group(1)

                # Extract any package name or component name overrides
                # within the block.
                # See caveat above.
                startpos = find.end()
                endpos = contents[startpos:].find("}") + startpos
                relevant_contents_section = contents[startpos:endpos]
                maybe_package_name_override = re.findall(
                    self._package_name_finder,
                    relevant_contents_section,
                )
                maybe_component_name_override = re.findall(
                    self._component_name_finder,
                    relevant_contents_section,
                )

                # Check first for component name override. Even though
                # we are finding packages, several Zircon tests
                # override package_name = component_name. Since we don't
                # actually evaluate BUILD.gn files, we assume that
                # package_name = component name to give reasonable
                # output from this matcher.
                if maybe_component_name_override:
                    package_name = maybe_component_name_override[0]
                if maybe_package_name_override:
                    package_name = maybe_package_name_override[0]

                package_target_path = f"{build_rule_prefix}:{target_name}"

                name_to_target[package_name].append(package_target_path)

                # See if any of the components in this file are included
                # as a test component of this file.
                for component_target, component_name in components_in_file:
                    test_component_list_start = relevant_contents_section.find(
                        "test_components"
                    )
                    test_component_list_end = -1
                    if test_component_list_start >= 0:
                        test_component_list_end = relevant_contents_section[
                            test_component_list_start:
                        ].find("]")
                        if test_component_list_end >= 0:
                            test_component_list_end += test_component_list_start

                    # If the test_components block contains the component
                    # target, we use the component name for matching
                    # and associate it with the *package* target.
                    if (
                        test_component_list_end >= 0
                        and relevant_contents_section[
                            test_component_list_start:test_component_list_end
                        ].find(component_target)
                        >= 0
                    ):
                        name_to_target[component_name].append(package_target_path)
        return dict(name_to_target)

    def find_matches(
        self, search_term: str, matcher: Matcher
    ) -> typing.List[Suggestion]:
        """Find matches within build files.

        Params:
            search_term     The value to search for
            matcher         A matcher object pre-configured with a threshold.

        Returns:
            A list of Suggestions from BUILD.gn files.
        """
        matches: typing.List[Suggestion] = []

        for name, targets in self._name_to_target.items():
            similarity = matcher.match(name, search_term)
            if similarity is not None:
                for target in sorted(set(targets)):  # deduplicate targets
                    matches.append(Suggestion(name, similarity, f"--with {target}"))

        return matches


class TestsFileMatcher:
    """Given a tests.json file path, supports searching for matching tests."""

    def __init__(self, tests_json_file: str):
        with open(tests_json_file, "r") as f:
            contents = json.load(f)
            self.names: typing.List[str] = [val["test"]["name"] for val in contents]

        # Match the name of Fuchsia packages.
        self._package_matcher = re.compile(r"fuchsia-pkg://fuchsia\.com/([^/#]+)#?")

    def __repr__(self):
        return f"TestFileMatcher(name_count={len(self.names)})"

    def find_matches(
        self,
        target: str,
        matcher: Matcher,
    ) -> typing.List[Suggestion]:
        """Find matches within a tests.json file.

        Parameters:
            target      The string to search for
            matcher     A Matcher object pre-configured with a threshold

        Returns:
            A list of Suggestions from the tests.json file.
        """
        matches: typing.List[Suggestion] = []
        for name in self.names:
            # Match on the entire package URL by default.
            options = [name]

            # If the name is a fuchsia-pkg URL, match on package name too.
            maybe_match = re.match(self._package_matcher, name)
            if maybe_match:
                options.append(maybe_match.group(1))

            # If the name is a file path, match on the last segment.
            # If the last segment ends in '.cm', strip that off.
            segments = name.split("/")
            if len(segments) > 1:
                if segments[-1][-3:] == ".cm":
                    segment = segments[-1][:-3]
                else:
                    segment = segments[-1]
                options.append(segment)

            # Get all scores above the matcher's threshold and their associated
            # name.
            scores = [
                y
                for x in options
                if (y := (matcher.match(x, target), x))[0] is not None
            ]
            (max_score, max_option) = max(scores) if scores else (None, None)
            if max_score is not None and max_option is not None:
                matches.append(
                    Suggestion(max_option, max_score, f"Build includes: {options[0]}")
                )

        return matches


def create_search_locations() -> SearchLocations:
    """Parses environment variables to produce SearchLocations"""

    fuchsia_directory = os.getenv("FUCHSIA_DIR")

    if not fuchsia_directory:
        raise Exception("Environment variable FUCHSIA_DIR must be set")
    elif not os.path.isdir(fuchsia_directory):
        raise Exception(f"Path {fuchsia_directory} should be a directory")

    with open(os.path.join(fuchsia_directory, ".fx-build-dir"), "r") as f:
        build_dir = os.path.join(fuchsia_directory, f.read().strip())

    tests_json_file = os.path.join(build_dir, "tests.json")
    if not os.path.isfile(tests_json_file):
        raise Exception(f"Expected to find a test list file at {tests_json_file}")

    return SearchLocations(fuchsia_directory, tests_json_file)


def main(args_list=None):
    parser = argparse.ArgumentParser(
        description="Fuzzy match and suggest tests in the Fuchsia build."
    )
    parser.add_argument(
        "match_string",
        help="The value to search for in your Fuchsia checkout.",
    )
    parser.add_argument(
        "--threshold",
        type=float,
        help="Threshold for matching, must be between 0.0 and 1.0",
        default=DEFAULT_THRESHOLD,
    )
    parser.add_argument(
        "--max-results",
        type=int,
        help="Maximum number of suggestions to display.",
        default=5,
    )
    parser.add_argument(
        "--omit-test-file",
        action="store_true",
        default=False,
        help="If true, do not include suggestions from tests.json",
    )
    parser.add_argument(
        "--debug",
        action="store_true",
        default=False,
        help="If true, print verbose timings for debugging.",
    )
    parser.add_argument(
        "--no-color",
        action="store_true",
        default=False,
        help="If set, do not use color output.",
    )

    args = parser.parse_args(args_list)

    return command(args)


if __name__ == "__main__":
    sys.exit(main())
