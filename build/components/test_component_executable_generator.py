# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse


def generate_bash_script(bin_path, test_pilot, output_filename):
    """
     Generates a Bash script that wraps execution of a test.

    Args:
        bin_path (str): The path to the FUCHSIA_BIN_PATH.
        test_pilot (str): The name of the test pilot.
        output_filename (str): The name of the output Bash script file.
    """

    with open(output_filename, "w") as f:
        f.write("#!/bin/bash\n")
        f.write("\n")
        f.write("FUCHSIA_BIN_PATH={} {}\n".format(bin_path, test_pilot))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Generate a Bash script with FUCHSIA_BIN_PATH and test pilot."
    )
    parser.add_argument(
        "--bin_path", required=True, help="The path to the FUCHSIA_BIN_PATH."
    )
    parser.add_argument(
        "--test_pilot", required=True, help="The name of the test pilot."
    )
    parser.add_argument(
        "--output_filename", required=True, help="Generated script name."
    )

    args = parser.parse_args()

    generate_bash_script(args.bin_path, args.test_pilot, args.output_filename)
