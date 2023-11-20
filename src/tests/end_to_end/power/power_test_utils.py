#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Fuchsia power test utility library."""

import csv
import json


def _avg(avg: float, value: float, count: int) -> float:
    return avg + (value - avg) / count


def _compute_metrics(
    sample: dict[object, object], metrics_dict: dict[object, object]
):
    n = metrics_dict["sampleCount"] + 1
    m = metrics_dict
    current = sample["current"]  # in milliAmpere
    voltage = sample["voltage"]  # in Volts
    power = voltage * current * 1e-3  # in Watts
    m["sampleCount"] = n
    m["maxPower"] = max(m["maxPower"], power)
    m["meanPower"] = _avg(m["meanPower"], power, n)
    m["minPower"] = min(m["minPower"], power)


def new_metrics() -> dict[object, object]:
    return {
        "sampleCount": 0,
        "maxPower": float("-inf"),
        "meanPower": 0,
        "minPower": float("inf"),
    }


def read_metrics(power_trace_path: str, metrics_dict: dict[object, object]):
    with open(power_trace_path, "r") as f:
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
            _compute_metrics(sample, metrics_dict)


def write_metrics(
    metric_name: str,
    metrics_dict: dict[object, object],
    fuchsiaperf_json_path: str,
):
    m = metrics_dict
    suite = f"fuchsia.power.{metric_name}"
    result = [
        {
            "label": "MinPower",
            "test_suite": suite,
            "unit": "Watts",
            "values": [m["minPower"]],
        },
        {
            "label": "MeanPower",
            "test_suite": suite,
            "unit": "Watts",
            "values": [m["meanPower"]],
        },
        {
            "label": "MaxPower",
            "test_suite": suite,
            "unit": "Watts",
            "values": [m["maxPower"]],
        },
    ]
    with open(fuchsiaperf_json_path, "w") as outfile:
        json.dump(result, outfile, indent=4)
