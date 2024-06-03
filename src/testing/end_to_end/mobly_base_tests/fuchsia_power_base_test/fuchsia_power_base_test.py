#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power measurement test class.

It is assumed that this is a hybrid test, with a host-side and a single target-side component.
"""

import json
import logging
import os
import signal
import subprocess
import time
from typing import Any

from fuchsia_base_test import fuchsia_base_test

from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaPowerBaseTest(fuchsia_base_test.FuchsiaBaseTest):
    """Fuchsia power measurement base test class.

    Single device test with power measurement.

    Attributes:
        fuchsia_devices: List of FuchsiaDevice objects.
        test_case_path: Directory pointing to a specific test case artifacts.
        snapshot_on: `snapshot_on` test param value converted into SnapshotOn
            Enum.
        power_trace_path: Path to power trace CSV.
        metric_name: Name of power metric being measured.

    Required Mobly Test Params:
        ffx_test_args (list[str]): Arguments to supply to `ffx test run`
        ffx_test_url (str): Test URL to execute via `ffx test run`
        timeout_sec (int): Test timeout.
        power_metric (str): Name of power metric being measured.
    """

    def setup_class(self) -> None:
        super().setup_class()
        self.metric_name = self.user_params["power_metric"]
        self.power_trace_path = os.path.join(
            self.log_path, f"{self.metric_name}_power_trace.csv"
        )
        self.ffx_test_url = self.user_params["ffx_test_url"]
        self.ffx_test_args = self.user_params["ffx_test_args"]
        self.timeout_sec = self.user_params["timeout_sec"]
        self.device: fuchsia_device.FuchsiaDevice = self.fuchsia_devices[0]  # type: ignore[name-defined]

    def _find_measurepower_path(self) -> str:
        path = os.environ.get("MEASUREPOWER_PATH")
        if not path:
            raise RuntimeError("MEASUREPOWER_PATH env variable must be set")
        return path

    def _wait_first_sample(self, proc: subprocess.Popen[Any]) -> None:
        for i in range(10):
            if proc.poll():
                stdout = proc.stdout.read() if proc.stdout else ""
                stderr = proc.stderr.read() if proc.stderr else ""
                raise RuntimeError(
                    f"Measure power failed to start with status "
                    f"{proc.returncode} stdout: {stdout} "
                    f"stderr: {stderr}"
                )
            if (
                os.path.isfile(self.power_trace_path)
                and os.path.getsize(self.power_trace_path) > 0
            ):
                return
            time.sleep(1)
        raise RuntimeError(
            f"Timed out while waiting to start power measurement"
        )

    def _start_power_measurement(self) -> subprocess.Popen[Any]:
        measurepower_path = self._find_measurepower_path()
        cmd = [
            measurepower_path,
            "-format",
            "csv",
            "-out",
            self.power_trace_path,
        ]
        _LOGGER.info(f"STARTING POWER MEASUREMENT: {cmd}")
        return subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )

    def _stop_power_measurement(self, proc: subprocess.Popen[Any]) -> None:
        _LOGGER.info(f"STOPPING POWER MEASUREMENT (process {proc.pid})")
        proc.send_signal(signal.SIGINT)
        result = proc.wait(60)
        if result != 0:
            stdout = proc.stdout.read() if proc.stdout else ""
            stderr = proc.stderr.read() if proc.stderr else ""
            raise RuntimeError(
                f"Measure power failed with status "
                f"{proc.returncode} stdout: {stdout} "
                f"stderr: {stderr}"
            )

    def test_launch_hermetic_test(self) -> None:
        """Executes a target-side workload while collecting power measurements.

        Power measurement result is streamed to |self.power_trace_path|.
        """

        def _component_is_running(moniker: str) -> bool:
            output = json.loads(
                self.device.ffx.run(
                    ["--machine", "json", "component", "show", moniker],
                )
            )
            return (
                "resolved" in output
                and "started" in output["resolved"]
                and output["resolved"]["started"] != None
            )

        with self._start_power_measurement() as proc:
            self._wait_first_sample(proc)
            ffx_test_args = self.ffx_test_args + [
                "--output-directory",
                self.test_case_path,
            ]
            load_generator_moniker = (
                "/core/ffx-laboratory:load_generator" + str(time.time())
            )
            try:
                # Before running the actual test, run component with a known load pattern. Since
                # power draw strongly correlates to cpu usage, we can use the pattern to match the
                # up the measurements.
                self.device.ffx.run(
                    [
                        "component",
                        "run",
                        load_generator_moniker,
                        "fuchsia-pkg://fuchsia.com/load_generator#meta/load_generator.cm",
                        "--config",
                        "load_pattern=[0,100,100,100,200,100,300]",
                    ],
                    capture_output=False,
                )

                # The load pattern will run for a second but ffx run will return immediately. Wait
                # for the load pattern to finish before proceeding.
                total_waited = 0
                while _component_is_running(load_generator_moniker):
                    if total_waited > 60:
                        raise RuntimeError(
                            "Failed to sucessfully launch and wait for the load generator"
                        )
                    time.sleep(1)
                    total_waited += 1

                self.device.ffx.run_test_component(
                    self.ffx_test_url,
                    ffx_test_args=ffx_test_args,
                    timeout=self.timeout_sec,
                    capture_output=False,
                )
            finally:
                self._stop_power_measurement(proc)
                self.device.ffx.run(
                    ["component", "destroy", load_generator_moniker],
                    capture_output=False,
                )


if __name__ == "__main__":
    test_runner.main()
