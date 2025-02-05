#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Generates schema for ffx commands"""

import argparse
import json
import os
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,  # Prepend helpdoc with this file's docstring.
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--command-list",
        type=str,
        required=True,
        help="Command list, one command per line to generate a schema for.",
    )
    parser.add_argument(
        "--schemalist",
        type=str,
        required=False,
        help=(
            "Destination file that will list all generated schema files."
            " If this argument is present, then the list is populated and "
            " no other args are processed"
        ),
    )
    parser.add_argument(
        "--goldens-dir",
        type=str,
        required=False,
        help="source directory for golden files.",
    )
    parser.add_argument(
        "--out-dir",
        type=str,
        required=False,
        help="output directory for schema information.",
    )
    parser.add_argument(
        "--sdk-root",
        type=str,
        required=False,
        help="sdk root to find ffx.",
    )
    parser.add_argument(
        "--comparisons",
        type=str,
        required=False,
        help="golden file test comparison file..",
    )
    args = parser.parse_args()

    if args.schemalist:
        build_schemalist(args.command_list, args.schemalist)
    else:
        if not args.comparisons:
            raise ValueError("--comparisons option is missing")
        if not args.sdk_root:
            raise ValueError("--sdk-root option is missing")
        if not args.out_dir:
            raise ValueError("--outdir option is missing")
        if not args.goldens_dir:
            raise ValueError("--goldens-dir option is missing")
        ffx_path = os.path.join(args.sdk_root, "tools/x64/ffx")
        build_command_list(
            args.command_list,
            args.out_dir,
            ffx_path,
            args.comparisons,
            args.goldens_dir,
            args.sdk_root,
        )


def build_schemalist(src_cmds, schemalist_path):
    with open(src_cmds) as input:
        with open(schemalist_path, mode="w") as output:
            for cmd in input.readlines():
                cmd = cmd.strip()
                cmd_parts = cmd.split(" ")
                schema_name = "_".join(cmd_parts[1:]) + ".json"
                print(schema_name, file=output)


def build_command_list(
    src_cmds, out_dir, ffx_path, comparison_path, goldens_dir, sdk_root
):
    comparisons = []
    cmd_diagnostics = None

    with open(src_cmds) as input:
        for cmd in input.readlines():
            cmd = cmd.strip()
            cmd_parts = cmd.split(" ")
            schema_name = "_".join(cmd_parts[1:]) + ".json"
            schema_out_path = os.path.join(out_dir, schema_name)
            comparisons.append(
                {
                    "golden": os.path.join(goldens_dir, schema_name),
                    "candidate": schema_out_path,
                }
            )
            schema_cmd = [
                ffx_path,
                "--no-environment",
                "-c",
                "sdk.module=host_tools_used_by_ffx_action_during_build",
                "-c",
                f"sdk.root={sdk_root}",
                "--schema",
                "--machine",
                "json-pretty",
            ] + cmd_parts[1:]
            with open(schema_out_path, "w") as schema_out:
                cmd_rc = subproce = subprocess.run(
                    schema_cmd, stdout=schema_out
                )
                if cmd_rc.returncode:
                    if not cmd_diagnostics:
                        cmds_cmd = [
                            ffx_path,
                            "-c",
                            "sdk.module=host_tools_used_by_ffx_action_during_build",
                            "-c",
                            f"sdk.root={sdk_root}",
                            "commands",
                        ]
                        cmd_diagnostics = subprocess.check_output(cmds_cmd)
                    raise ValueError(
                        f"Error running {schema_cmd}: {cmd_rc.returncode} {cmd_rc.stdout} {cmd_rc.stderr}\nAll commands {cmd_diagnostics}"
                    )

    with open(comparison_path, mode="w") as cmp_file:
        json.dump(comparisons, cmp_file)


if __name__ == "__main__":
    sys.exit(main())
