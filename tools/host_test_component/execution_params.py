# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
from dataclasses import dataclass
from typing import List, Self

TEST_URL_KEY = "test_url"
TEST_ARGS_KEY = "test_args"
TEST_FILTERS_KEY = "test_filters"
RUN_DISABLED_TESTS_KEY = "run_disabled_tests"
PARALLEL_KEY = "parallel"
MAX_SEVERITY_LOGS_KEY = "max_severity_logs"
REALM_KEY = "realm"


@dataclass
class ExecutionParams:
    """Class to parse and store execution parameters"""

    test_url: str
    test_args: List[str]
    test_filters: List[str]
    run_disabled_tests: bool
    parallel: str
    max_severity_logs: str
    realm: str

    @classmethod
    def initialize_from_json(cls, json_str: str) -> Self:
        """Initialize and validate params from standard json format.

        Args:
            json_str (str): standard json format containing arguments to run tests.

        Raises:
            ValueError: Validation error

        Returns:
            Self
        """
        # Parse the JSON string
        json_data = json.loads(json_str)

        # Check if 'test_url' is missing in the JSON data
        if TEST_URL_KEY not in json_data:
            raise ValueError(f"Missing '{TEST_URL_KEY}' in the JSON data")

        # Extract fields from the JSON data
        test_url = json_data[TEST_URL_KEY]
        test_args = json_data.get(TEST_ARGS_KEY, [])
        test_filters = json_data.get(TEST_FILTERS_KEY, [])
        run_disabled_tests = json_data.get(RUN_DISABLED_TESTS_KEY, False)
        parallel = json_data.get(PARALLEL_KEY, None)
        max_severity_logs = json_data.get(MAX_SEVERITY_LOGS_KEY, None)
        realm = json_data.get(REALM_KEY, None)

        return cls(
            test_url,
            test_args,
            test_filters,
            run_disabled_tests,
            parallel,
            max_severity_logs,
            realm,
        )
