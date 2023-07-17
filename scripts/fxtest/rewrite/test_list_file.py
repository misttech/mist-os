# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Data model and associated methods for test-list.json files.

test-list.json is a post-processed version of tests.json that is
produced by //tools/test_list_tool. It provides basic execution
instructions and categorization for tests that are part of the
Fuchsia build.

test-list.json is typically stored at the root of the output directory
for the Fuchsia build.
"""

from dataclasses import dataclass
import json
import typing

from dataparse import dataparse
import tests_json_file


@dataparse
@dataclass
class TestListTagKV:
    """Key/Value pair for test tags."""

    key: str
    value: str


@dataparse
@dataclass
class TestListExecutionEntry:
    """The entry describing how to execute a test in test-list.json."""

    # If set, this is the component URL to execute using ffx test.
    component_url: str

    # If set, this is the --realm argument for ffx test.
    realm: typing.Optional[str] = None

    # If set, this is the --max-severity-logs argument for ffx test.
    max_severity_logs: typing.Optional[str] = None

    # If set, this is the --min-severity-logs argument for ffx test.
    min_severity_logs: typing.Optional[str] = None


@dataparse
@dataclass
class TestListEntry:
    """A single test entry stored in test-list.json."""

    # The name of the test. Must be unique in the file.
    name: str

    # A list of tags for the test, stored as KV pairs.
    tags: typing.List[TestListTagKV]

    # Execution details for the test.
    execution: typing.Optional[TestListExecutionEntry] = None

    def is_hermetic(self) -> bool:
        """Determine if a test is hermetic.

        A test is hermetic only if it has a tag "hermetic" set to
        "true". Otherwise we assume the test may be non-hermetic.

        Hermetic tests run in isolation, so knowing that a test is hermetic means
        we can run that test in parallel with other tests safely.

        Returns:
            bool: True if the test is definitely hermetic, False otherwise.
        """
        for tag in self.tags:
            if tag.key == "hermetic" and tag.value == "true":
                return True
        return False


@dataparse
@dataclass
class TestListFile:
    """Top-level data model for test-list.json files."""

    # The list of test list entries stored in the file.
    data: typing.List[TestListEntry]

    @staticmethod
    def entries_from_file(file: str) -> typing.Dict[str, TestListEntry]:
        """Parse the file at the given path as a test-list.json file.

        This method converts the flat list in the file into a dictionary
        keyed by test name.

        Args:
            file (os.PathLike): The file path to parse.

        Returns:
            typing.Dict[str, TestListEntry]: Map from test name to entry for that test in the file.

        Raises:
            IOError: If the file could not be read.
            JSONDecodeError: If the file is not valid JSON.
            DataParseError: If the data in the file does not match the data model.
        """
        with open(file) as f:
            data = json.load(f)
            parsed: TestListFile = TestListFile.from_dict(data)  # type:ignore
            ret = dict()
            for value in parsed.data:
                ret[value.name] = value
        return ret


Self = typing.TypeVar("Self", bound="Test")


@dataclass
class Test:
    """Wrapper containing data from both tests.json and test-list.json.

    tests.json contains build-specific information about tests,
    while test-list.json provides the necessary information to
    execute and categorize tests. Since both pieces of information
    are needed to make sense of tests, this dataclass combines both
    pieces for a test.
    """

    # The test as described by tests.json.
    build: tests_json_file.TestEntry

    # The test as described by test-list.json.
    info: TestListEntry

    def __hash__(self) -> int:
        return self.info.name.__hash__()

    def __eq__(self, other) -> bool:
        return self.build.__eq__(other)

    @staticmethod
    def join_test_descriptions(
        test_entries: typing.List[tests_json_file.TestEntry],
        test_list_entries: typing.Dict[str, TestListEntry],
    ) -> typing.List[Self]:
        """Join the contents of tests.json with the contents of test-list.json.

        Args:
            test_entries (List[TestEntry]): The contents parsed from tests.json.
            test_list_entries (Dict[str, TestListEntry]): The contents parsed from test-list.json

        Raises:
            ValueError: If a test from tests.json does not have a corresponding entry in test-list.json.

        Returns:
            List[Test]: List of joined contents for all tests in tests.json.
        """
        try:
            ret: typing.List[Test] = [
                Test(entry, test_list_entries[entry.test.name])
                for entry in test_entries
            ]
            # Ignore type for now. With Python 3.11 we can use typing.Self.
            return ret  # type:ignore
        except KeyError as e:
            raise ValueError(
                f"Test '{e.args[0]} was found in "
                + "tests.json, but not test-list.json. This may be an error "
                + "with your build configuration."
            )
