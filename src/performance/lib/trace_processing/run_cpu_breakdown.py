#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Returns CPU breakdown in a JSON file.
"""

import argparse
import os.path
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import cpu_breakdown
import sys


def main() -> None:
    """
    Takes in a trace file in JSON format.
    """
    parser = argparse.ArgumentParser()
    parser.add_argument("path_to_json")
    args = parser.parse_args()

    model: trace_model.Model = trace_importing.create_model_from_file_path(
        args.path_to_json
    )
    cpu_breakdown.CpuBreakdownMetricsProcessor(model).process_metrics()


if __name__ == "__main__":
    main()
