#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Returns CPU breakdown in a JSON file.
"""

import argparse
import pathlib
import pprint
from trace_processing import trace_importing, trace_metrics, trace_model
from trace_processing.metrics import cpu_breakdown
import sys


# Default cut-off for the percentage CPU. Any process that has CPU below this
# won't be listed in the results. User can pass in a cutoff.
DEFAULT_PERCENT_CUTOFF = 0.0


def main() -> None:
    """
    Takes in a trace file in JSON format and outputs
    a free-form JSON with breakdown metrics to `output_path`.
    """
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "path_to_trace",
        type=str,
        help="path to trace file, either .fxt or .json",
    )
    parser.add_argument(
        "output_path", type=str, help="output path for CPU breakdown, as .json"
    )
    parser.add_argument(
        "--percent_cutoff",
        type=float,
        default=DEFAULT_PERCENT_CUTOFF,
        help="percent cutoff (CPU usage less than this percent will not be logged)",
    )
    args = parser.parse_args()

    trace_path_json: str = args.path_to_trace

    if args.path_to_trace.endswith(".fxt"):
        trace2json_path = pathlib.Path(sys.argv[0]).parent / "trace2json"
        trace_path_json = trace_importing.convert_trace_file_to_json(
            trace_path=args.path_to_trace,
            trace2json_path=trace2json_path,
        )
    elif not args.path_to_trace.endswith(".json"):
        parser.print_help()
        return

    model: trace_model.Model = trace_importing.create_model_from_file_path(
        trace_path_json
    )
    breakdown = cpu_breakdown.CpuBreakdownMetricsProcessor(
        model, args.percent_cutoff
    ).process_metrics()

    with open(args.output_path, "w") as json_file:
        pprint.pprint(breakdown, json_file)


if __name__ == "__main__":
    main()
