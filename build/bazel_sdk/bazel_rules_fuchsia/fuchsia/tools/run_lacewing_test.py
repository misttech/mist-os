# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import shutil
import shlex
import subprocess
import sys
import tempfile

from pathlib import Path
from typing import Tuple, List, Optional


# `bazel test` incorrectly handles stdout, so logging to stderr will keep our
# log statements in order w.r.t test/subprocess output.
# See https://github.com/bazelbuild/bazel/issues/7388.
def log(*kwargs):
    print(*kwargs, file=sys.stderr)


def parse_args() -> Tuple[argparse.Namespace, List[str]]:
    """Separates relevant arguments from unknown arguments."""
    parser = argparse.ArgumentParser()

    def path_arg(type="file"):
        def arg(path):
            path = Path(path)
            assert (
                type == "file"
                and path.is_file()
                or type == "directory"
                and path.is_dir()
            ), f'Path "{path}" is not a {type}!'
            return path

        return arg

    parser.add_argument(
        "--name",
        help="The test's name.",
        required=True,
    )
    parser.add_argument(
        "--test-pyz",
        type=path_arg(),
        help="A path to the prebuilt lacewing test pyz.",
        required=True,
    )
    parser.add_argument(
        "--cwd",
        type=path_arg("directory"),
        help="The cwd to run the pyz under.",
        required=True,
    )
    parser.add_argument(
        "--ffx",
        type=path_arg(),
        help="A path to the ffx tool.",
        required=True,
    )
    parser.add_argument(
        "--target",
        help="Optionally specify the target to run these tests against. Defaults to the default target device.",
    )
    return parser.parse_known_args()


def write_mobly_config(test_bed: str, ffx: Path, target: Optional[str]) -> Path:
    target = (
        target
        or subprocess.check_output(
            [ffx, "target", "default", "get"],
            text=True,
        ).strip()
    )
    mobly_config_contents = json.dumps(
        {
            "TestBeds": [
                {
                    "Name": test_bed,
                    "Controllers": {
                        "FuchsiaDevice": [
                            {
                                "name": target,
                                "transport": "fuchsia-controller",
                                "ffx_path": str(ffx.resolve()),
                            }
                        ]
                    },
                }
            ]
        },
        indent=2,
    )
    mobly_config = Path(tempfile.mktemp(".json", "mobly_config-"))
    mobly_config.write_text(mobly_config_contents)
    log(f"DEBUG: {str(mobly_config)}:\n{mobly_config_contents}")
    return mobly_config


def run_lacewing_test(
    cwd: Path,
    test_pyz: Path,
    mobly_config: Path,
    *test_args: str,
) -> int:
    # Set FIDL_IR_PATH.
    if not (cwd / "ir_root").is_dir():
        log(
            f"WARNING: FIDL_IR_PATH directory {str(cwd)}/ir_root does not exist."
        )
    env = os.environ | {"FIDL_IR_PATH": str(cwd / "ir_root")}

    # Run the test.
    command = [
        sys.executable,
        test_pyz.resolve(),
        "-c",
        mobly_config,
        *test_args,
    ]
    log(f"DEBUG: Running subcommand: {shlex.join(map(str, command))}")
    log(f"DEBUG: Subcommand working directory: {cwd}")
    return subprocess.run(command, cwd=cwd, env=env).returncode


def copy_lacewing_outputs(test_bed: str, output_path: Path) -> None:
    log(f"DEBUG: Copying test outputs for {test_bed}: {output_path}")
    shutil.copytree(f"/tmp/logs/mobly/{test_bed}/latest", output_path)


def main() -> int:
    args, forward_args = parse_args()

    test_bed = "GeneratedTestbed"

    # Write the mobly config.
    mobly_config = write_mobly_config(test_bed, args.ffx, args.target)

    # Run the lacewing test.
    result = run_lacewing_test(
        cwd=args.cwd.resolve(),
        test_pyz=args.test_pyz.resolve(),
        mobly_config=mobly_config,
        *forward_args,
    )

    # Upload lacewing's test output artifacts.
    # See `bazel_build_test_upload` for how this environment variable is used.
    if "TEST_UNDECLARED_OUTPUTS_DIR" in os.environ:
        copy_lacewing_outputs(
            test_bed,
            Path(os.environ["TEST_UNDECLARED_OUTPUTS_DIR"]) / args.name,
        )
    return result


if __name__ == "__main__":
    sys.exit(main())
