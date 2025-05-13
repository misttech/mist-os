# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import os
import time
from abc import ABC, abstractmethod
from typing import Any, Generic, TypeVar

P = TypeVar("P")
R = TypeVar("R")


class ActionTimer(ABC, Generic[P, R]):
    """Base class that times an action and records the time to a file.

    This utility class allows a subclass to execute an action 1 or more times,
    capture the wallclock time taken for each iteration, and record them to a
    file.

    Duration measurements will be recorded to a Fuchsia freeform metrics file,
    with the suffix `freeform.json`.
    """

    def execute(
        self,
        test_suite: str,
        label: str,
        results_path: str,
        repetitions: int = 1,
    ) -> None:
        """Executes and measures the duration of an action.

        Args:
            test_suite: the name of the test suite for the output
                freeform.json.
            label: the label for the test within the test suite.
            results_path: the path where the freeform.json file will be
                written.
            repetitions: the number of times to execute an action.
        """
        time_ns: list[int] = []
        self.additional_json: list[Any] = []
        for _ in range(repetitions):
            pre_result = self.pre_action()
            start = time.monotonic_ns()
            result = self.action(pre_result)
            dur = time.monotonic_ns() - start
            time_ns.append(dur)
            self.post_action(result)
        self._write_to_file(test_suite, label, results_path, time_ns)

    @abstractmethod
    def pre_action(self) -> P:
        """Set up code to execute before the action.

        The duration of this method won't be measured.

        The output of this method will be passed to the action when executing.
        """

    @abstractmethod
    def action(self, pre_result: P) -> R:
        """The action which duration will be measured.

        The duration of this method will be measured.

        The output of this method will be passed to the post_action method when
        executing.

        Args:
            pre_result: the output of self.pre_action.
        """

    @abstractmethod
    def post_action(self, step_output: R) -> None:
        """Executed after the action.

        The duration of this method won't be measured.

        Args:
            step_output: the output of self.action.
        """

    def _write_to_file(
        self,
        test_suite: str,
        label: str,
        results_path: str,
        time_ns: list[int],
    ) -> None:
        fuchsiaperf_data = [
            {
                "test_suite": test_suite,
                "label": label,
                "values": time_ns,
                "unit": "ns",
            },
        ]
        fuchsiaperf_data.extend(self.additional_json)
        test_perf_file = os.path.join(
            results_path, f"results.{label.lower()}.freeform.json"
        )
        with open(test_perf_file, "w") as f:
            json.dump(fuchsiaperf_data, f, indent=4)
