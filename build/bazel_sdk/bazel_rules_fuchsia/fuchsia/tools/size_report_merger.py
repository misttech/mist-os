# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Merge Size reports"""

import argparse
import json
import os


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--size-budgets",
        help="Paths to size budgets, separated by comma",
    )
    parser.add_argument(
        "--size-reports",
        help="Paths to size-reports, separated by comma",
    )
    parser.add_argument(
        "--verbose-outputs",
        help="Paths to verbose outputs, separated by comma",
    )
    parser.add_argument(
        "--merged-size-budgets",
        required=True,
    )
    parser.add_argument(
        "--merged-size-reports",
        required=True,
    )
    parser.add_argument(
        "--merged-verbose-outputs",
        required=True,
    )
    return parser.parse_args()


def main():
    args = parse_args()
    size_budget_table = {"package_set_budgets": []}
    if args.size_budgets:
        for size_budget in args.size_budgets.split(","):
            with open(size_budget, "r") as f:
                budgets = json.load(f)
                size_budget_table["package_set_budgets"].extend(
                    budgets["package_set_budgets"]
                )
    size_report_table = {}
    if args.size_reports:
        for size_report in args.size_reports.split(","):
            with open(size_report, "r") as f:
                size_report_table.update(json.load(f))
    verbose_output_table = {}
    if args.verbose_outputs:
        for verbose_output in args.verbose_outputs.split(","):
            with open(verbose_output, "r") as f:
                verbose_output_table.update(json.load(f))

    with open(args.merged_size_budgets, "w") as f:
        json.dump(size_budget_table, f, indent=4)

    with open(args.merged_size_reports, "w") as f:
        json.dump(size_report_table, f, indent=4)

    with open(args.merged_verbose_outputs, "w") as f:
        json.dump(verbose_output_table, f, indent=4)


if __name__ == "__main__":
    main()
