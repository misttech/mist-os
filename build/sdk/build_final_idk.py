#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Build the final Fuchsia Base IDK archive, by merging the result for a
top-level IDK build with those of extra CPU-specific sub-builds."""

import argparse
import collections
import os
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Set, Tuple

_DEFAULT_BUILD_DIR_PREFIX = "fuchsia-idk-build-"

_ARGS_GN_TEMPLATE = r"""# Auto-generated - DO NOT EDIT
import("//products/bringup.gni")
import("//boards/{cpu}.gni")

build_info_board = "{cpu}"
build_info_product = "bringup"
is_debug = false

cxx_rbe_enable = {cxx_rbe_enable}
rust_rbe_enable = {rust_rbe_enable}
use_goma = {use_goma}
universe_package_labels += [{sdk_labels_list}]
"""


def get_host_platform() -> str:
    """Return host platform name, following Fuchsia conventions."""
    if sys.platform == "linux":
        return "linux"
    elif sys.platform == "darwin":
        return "mac"
    else:
        return os.uname().sysname


def get_host_arch() -> str:
    """Return host CPU architecture, following Fuchsia conventions."""
    host_arch = os.uname().machine
    if host_arch == "x86_64":
        return "x64"
    elif host_arch.startswith(("armv8", "aarch64")):
        return "arm64"
    else:
        return host_arch


def get_host_tag() -> str:
    """Return host tag, following Fuchsia conventions."""
    return "%s-%s" % (get_host_platform(), get_host_arch())


def log(msg: str):
    """Print log message to stderr."""
    print("LOG: " + msg, file=sys.stderr)


def error(msg: str) -> int:
    """Print error message to stderr, then return 1."""
    print("ERROR: " + msg, file=sys.stderr)
    return 1


def write_file_if_unchanged(path: Path, content: str) -> bool:
    """Write |content| into |path| if needed. Return True on write."""
    if path.exists() and path.read_text() == content:
        # Nothing to do
        return False

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    return True


def run_command(args: List, cwd=None):
    """Run a command.

    Args:
        args: A list of strings or Path items (each one of them will be
            converted to a string for convenience).
        cwd: If not None, path to the directory where to run the command.
    Returns:
        a subprocess.run() result value.
    """
    cmd_args = [str(a) for a in args]
    cmd_str = " ".join(shlex.quote(a) for a in cmd_args)
    log(f"RUN: {cmd_str}")
    start_time = time.time()
    result = subprocess.run([str(c) for c in cmd_args], cwd=cwd)
    end_time = time.time()
    log('DURATION: %.1fs' % (end_time - start_time))
    return result


def run_checked_command(args: List, cwd=None):
    try:
        ret = run_command(args, cwd)
        ret.check_returncode()
        return False
    except KeyboardInterrupt:
        return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--output-dir", help="Output IDK directory.")
    group.add_argument("--output-archive", help="Output IDK archive.")
    parser.add_argument(
        "--sdk-targets",
        nargs="+",
        required=True,
        help="List of GN sdk() targets to build.",
    )
    parser.add_argument(
        "--base-build-dir",
        required=True,
        help="Build directory containing host tools and host_cpu atoms.")
    parser.add_argument(
        "--extra-target-cpus",
        nargs="+",
        required=True,
        help="List of extra target CPU names.")
    parser.add_argument("--stamp-file", help="Optional output stamp file.")
    parser.add_argument(
        "--fuchsia-dir", help="Specify Fuchsia source directory.")
    parser.add_argument(
        "--build-dir-prefix",
        default=_DEFAULT_BUILD_DIR_PREFIX,
        help=
        f"Specify intermediate build directory prefix (default {_DEFAULT_BUILD_DIR_PREFIX})",
    )
    parser.add_argument(
        "--cxx-rbe-enable",
        action="store_true",
        help="Enable remote builds with RBE for C++ targets.",
    )
    parser.add_argument(
        "--rust-rbe-enable",
        action="store_true",
        help="Enable remote builds with RBE for Rust targets.",
    )
    parser.add_argument(
        "--use-goma", action="store_true", help="Whether to use goma or not.")
    parser.add_argument(
        "--goma-dir",
        help="Optional goma directory path, only used with --use-goma.")
    parser.add_argument(
        "--host-tag", help="Fuchsia host os/cpu tag used to find prebuilts.")
    parser.add_argument(
        "--clean", action="store_true", help="Force clean build.")
    args = parser.parse_args()

    if args.fuchsia_dir:
        fuchsia_dir = Path(args.fuchsia_dir)
    else:
        # Assume this script is under //build/sdk/...
        fuchsia_dir = Path(__file__).parent.parent.parent

    build_dir_prefix = args.build_dir_prefix
    if not build_dir_prefix:
        parser.error("--build-dir-prefix value cannot be empty!")

    # Locate GN and Ninja prebuilts.
    if args.host_tag:
        host_tag = args.host_tag
    else:
        host_tag = get_host_tag()

    gn_path = fuchsia_dir / "prebuilt" / "third_party" / "gn" / host_tag / "gn"
    if not gn_path.exists():
        return error(f"Missing gn prebuilt binary: {gn_path}")

    ninja_path = fuchsia_dir / "prebuilt" / "third_party" / "ninja" / host_tag / "ninja"
    if not ninja_path.exists():
        return error(f"Missing ninja prebuilt binary: {ninja_path}")

    base_build_dir = Path(args.base_build_dir)

    # This script creates CPU-specific sub-builds under:
    #
    #   $BUILD_DIR/fuchsia-idk-build-$CPU/
    #
    # In each build, generating an exported sdk() target populates a directory
    # like:
    #
    #   $BUILD_DIR/fuchsia-idk-build-$CPU/sdk/exported/<name>/
    #
    # With a tree of symlinks to build artifacts that are in other parts
    # of $BUILD_DIR/fuchsia-idk-sub-build-$CPU
    #
    # This script will merge all these into $OUTPUT_DIR.
    #
    # Parse --sdk-targets GN labels and record related information for each entry.
    def sdk_label_partition(target_label: str) -> Tuple[str, str]:
        """Split an SDK GN label into a (dir, name) pair."""
        # Expected format is //<dir>:<name>
        path, colon, name = target_label.partition(":")
        assert colon == ":" and path.startswith("//"), (
            f'Invalid SDK target label: {target_label}')
        return (path[2:], name)

    def sdk_label_to_ninja_target(target_label: str) -> str:
        """Convert SDK GN label to Ninja target path."""
        target_dir, target_name = sdk_label_partition(target_label)
        return f'{target_dir}:{target_name}'

    def sdk_label_to_exported_dir(target_label: str, build_dir: Path) -> Path:
        """Convert SDK GN label to exported directory in build_dir."""
        target_dir, target_name = sdk_label_partition(target_label)
        return build_dir / 'sdk' / 'exported' / target_name

    # Compute the list of Ninja targets to build in each sub-build.
    ninja_targets = [sdk_label_to_ninja_target(l) for l in args.sdk_targets]
    log('Ninja targets to build: %s' % ' '.join(sorted(ninja_targets)))

    # The list of all input directories for the final merge operation.
    # Start by adding all export SDK directories from the main build dir.
    all_input_dirs: List[Path] = []
    for sdk_target in args.sdk_targets:
        base_exported_dir = sdk_label_to_exported_dir(
            sdk_target, base_build_dir)
        if not base_exported_dir.exists():
            parser.error(
                f'Required base directory does not exist: {base_exported_dir}')
        all_input_dirs.append(str(base_exported_dir))

    for target_cpu in args.extra_target_cpus:
        build_dir = Path(build_dir_prefix + target_cpu)
        build_dir.mkdir(exist_ok=True, parents=True)
        log(
            f'{build_dir}: Preparing sub-build, directory: {build_dir.resolve()}'
        )

        if args.clean and build_dir.exists():
            log(f'{build_dir}: Cleaning build directory')
            run_command([ninja_path, "-C", build_dir, "-t", "clean"])

        log(f'{build_dir}: Generating GN/Ninja build plan.')

        args_gn_content = _ARGS_GN_TEMPLATE.format(
            cpu=target_cpu,
            cxx_rbe_enable="true" if args.cxx_rbe_enable else "false",
            rust_rbe_enable="true" if args.rust_rbe_enable else "false",
            use_goma="true" if args.use_goma else "false",
            sdk_labels_list=', '.join(f'"{l}"' for l in args.sdk_targets),
        )
        if args.use_goma and args.goma_dir:
            args_gn_content += 'goma_dir = "%s"\n' % args.goma_dir

        # Only build host tools in the x64 sub-build, to save
        # considerable time.
        args_gn_content += "sdk_no_host_tools = true\n"

        if write_file_if_unchanged(build_dir / 'args.gn', args_gn_content):
            if run_checked_command([gn_path,
                                    "--root=%s" % fuchsia_dir.resolve(), "gen",
                                    build_dir]):
                return 1

        log(f'{build_dir}: Generating IDK sub-targets')
        if run_checked_command([ninja_path, "-C", build_dir] + ninja_targets):
            return 1

        for sdk_target in args.sdk_targets:
            all_input_dirs.append(
                sdk_label_to_exported_dir(sdk_target, build_dir))

    # Merge everything into the final directory (or archive).
    merge_cmd_args = [
        sys.executable,
        "-S",
        fuchsia_dir / "scripts" / "sdk" / "merger" / "merge.py",
    ]
    for input_dir in all_input_dirs:
        merge_cmd_args += ["--input-directory", str(input_dir)]

    if args.output_dir:
        merge_cmd_args += ["--output-directory", args.output_dir]
    else:
        merge_cmd_args += ["--output-archive", args.output_archive]

    if run_checked_command(merge_cmd_args):
        return 1

    # Write stamp file if needed.
    if args.stamp_file:
        with open(args.stamp_file, "w") as f:
            f.write("")

    return 0


if __name__ == "__main__":
    sys.exit(main())
