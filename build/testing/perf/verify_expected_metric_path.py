# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
""" This script generates the component manifest used by the validation-client component."""

import argparse
import os.path

EXPECTED_METRICS_DIR_PATH = os.path.join(
    "tests", "end_to_end", "perf", "expected_metric_names"
)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--expected-metric-names-filepath",
        help="Path to the expected metrics name file",
        required=True,
    )
    parser.add_argument(
        "--output-file", help="Path to the verification file", required=True
    )
    args = parser.parse_args()

    metric_names_filepath = args.expected_metric_names_filepath
    dir_path = os.path.dirname(metric_names_filepath)
    if not dir_path.endswith(EXPECTED_METRICS_DIR_PATH):
        raise Exception(
            f"Directory containing expected metric file must end in "
            '"{EXPECTED_METRICS_DIR_PATH}": {metric_names_filepath}'
        )

    with open(args.output_file, "w") as f:
        f.write("Verified!\n")


if __name__ == "__main__":
    sys.exit(main())
