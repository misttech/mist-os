#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power measurement test class.

It is assumed that this is a hybrid test, with a host-side and a single target-side component.
"""

import csv
import json
import logging
import os
import signal
import subprocess
import time

from fuchsia_base_hybrid_test_lib import fuchsia_hybrid_base_test
from mobly import test_runner

_LOGGER: logging.Logger = logging.getLogger(__name__)


class FuchsiaPowerBaseTest(fuchsia_hybrid_base_test.FuchsiaHybridBaseTest):
    """Fuchsia power measurement base test class.

    Single device hybrid test with power measurement.

    Attributes:
        fuchsia_devices: List of FuchsiaDevice objects.
        test_case_path: Directory pointing to a specific test case artifacts.
        snapshot_on: `snapshot_on` test param value converted into SnapshotOn
            Enum.

    Required Mobly Test Params:
        ffx_test_options(list[str]): Test options to supply to `ffx test run`
        ffx_test_url (str): Test URL to execute via `ffx test run`
        timeout_sec (int): Test timeout.
    """

    def __init__(self, config):
        super().__init__(config)
        self._metrics = {
            "sampleCount": 0,
            "avgCurrent": 0,
            "minCurrent": float("inf"),
            "maxCurrent": float("-inf"),
            "avgVoltage": 0,
            "minVoltage": float("inf"),
            "maxVoltage": float("-inf"),
            "avgPower": 0,
            "minPower": float("inf"),
            "maxPower": float("-inf"),
        }

    def _find_measurepower_path(self):
        path = os.environ.get("MEASUREPOWER_PATH")
        if not path:
            raise RuntimeError("MEASUREPOWER_PATH env variable must be set")
        return path

    def _wait_first_sample(self, proc, out_path):
        for i in range(10):
            if proc.poll():
                stdout = proc.stdout.read()
                stderr = proc.stderr.read()
                raise RuntimeError(
                    f"Measure power failed to start with status "
                    f"{proc.returncode} stdout: {stdout} "
                    f"stderr: {stderr}"
                )
            if os.path.isfile(out_path) and os.path.getsize(out_path) > 0:
                return
            time.sleep(1)
        raise RuntimeError(
            f"Timed out while waiting to start power measurement"
        )

    def _start_power_measurement(self, out_path):
        measurepower_path = self._find_measurepower_path()
        cmd = [measurepower_path, "-format", "csv", "-out", out_path]
        _LOGGER.info(f"STARTING POWER MEASUREMENT: {cmd}")
        return subprocess.Popen(
            cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )

    def _stop_power_measurement(self, proc: subprocess.Popen):
        _LOGGER.info(f"STOPPING POWER MEASUREMENT (process {proc.pid})")
        proc.send_signal(signal.SIGINT)
        result = proc.wait(60)
        if result:
            stdout = proc.stdout.read()
            stderr = proc.stderr.read()
            raise RuntimeError(
                f"Measure power failed with status "
                f"{proc.returncode} stdout: {stdout} "
                f"stderr: {stderr}"
            )

    def _read_metrics(self, out_path):
        with open(out_path, "r") as f:
            reader = csv.reader(f)
            header = next(reader)
            assert header[0] == "Timestamp"
            assert header[1] == "Current"
            assert header[2] == "Voltage"
            for row in reader:
                sample = {
                    "timestamp": int(row[0]),
                    "current": float(row[1]),
                    "voltage": float(row[2]),
                }
                self._compute_metrics(sample)

    def _avg(self, avg, value, count):
        return avg + (value - avg) / count

    def _compute_metrics(self, sample):
        n = self._metrics["sampleCount"] + 1
        m = self._metrics
        current = sample["current"]  # in milliAmpere
        voltage = sample["voltage"]  # in Volts
        power = voltage * current * 1e-3  # in Watts
        self._metrics = {
            "sampleCount": n,
            "avgCurrent": self._avg(m["avgCurrent"], current, n),
            "minCurrent": min(m["minCurrent"], current),
            "maxCurrent": max(m["maxCurrent"], current),
            "avgVoltage": self._avg(m["avgVoltage"], voltage, n),
            "minVoltage": min(m["minVoltage"], voltage),
            "maxVoltage": max(m["maxVoltage"], voltage),
            "avgPower": self._avg(m["avgPower"], power, n),
            "minPower": min(m["minPower"], power),
            "maxPower": max(m["maxPower"], power),
        }

    def _write_metrics(self):
        m = self._metrics
        suite = f"fuchsia.power_metric.{self.current_test_info.name}"
        result = [
            {
                "label": "SampleCount",
                "test_suite": suite,
                "unit": "Count",
                "values": [m["sampleCount"]],
            },
            {
                "label": "AvgCurrent",
                "test_suite": suite,
                "unit": "mA",
                "values": [m["avgCurrent"]],
            },
            {
                "label": "MinCurrent",
                "test_suite": suite,
                "unit": "mA",
                "values": [m["minCurrent"]],
            },
            {
                "label": "MaxCurrent",
                "test_suite": suite,
                "unit": "mA",
                "values": [m["maxCurrent"]],
            },
            {
                "label": "AvgVoltage",
                "test_suite": suite,
                "unit": "V",
                "values": [m["avgVoltage"]],
            },
            {
                "label": "MinVoltage",
                "test_suite": suite,
                "unit": "V",
                "values": [m["minVoltage"]],
            },
            {
                "label": "MaxVoltage",
                "test_suite": suite,
                "unit": "V",
                "values": [m["maxVoltage"]],
            },
            {
                "label": "AvgPower",
                "test_suite": suite,
                "unit": "W",
                "values": [m["avgPower"]],
            },
            {
                "label": "MinPower",
                "test_suite": suite,
                "unit": "W",
                "values": [m["minPower"]],
            },
            {
                "label": "MaxPower",
                "test_suite": suite,
                "unit": "W",
                "values": [m["maxPower"]],
            },
        ]
        out_path = os.path.join(
            self.log_path,
            f"{self.current_test_info.name}_power_metrics.fuchsiaperf.json",
        )
        with open(out_path, "w") as outfile:
            json.dump(result, outfile, indent=4)

    def test_launch_hermetic_test(self) -> None:
        out_path = os.path.join(
            self.log_path, f"{self.current_test_info.name}_power_trace.csv"
        )
        with self._start_power_measurement(out_path) as proc:
            self._wait_first_sample(proc, out_path)
            super().test_launch_hermetic_test()
            self._stop_power_measurement(proc)
            self._read_metrics(out_path)
            self._write_metrics()


if __name__ == "__main__":
    test_runner.main()
