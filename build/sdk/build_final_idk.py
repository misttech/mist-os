#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Build the final Fuchsia Base IDK archive, by performing several local
sub-builds and merging the results."""

import argparse
import collections
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Dict, List

_DEFAULT_BUILD_DIR_NAME = "fuchsia-idk-sub-build"

_ARGS_GN_TEMPLATE = r"""# Auto-generated - DO NOT EDIT
import("//products/bringup.gni")
import("//boards/{cpu}.gni")

build_info_board = "{cpu}"
build_info_product = "bringup"
is_debug = false

cxx_rbe_enable = {cxx_rbe_enable}
rust_rbe_enable = {rust_rbe_enable}
use_goma = {use_goma}
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


def my_relpath(path: Path) -> Path:
    """Return input path, relative to the current directory."""
    # Only needed because Path.relative_to() does not work in all cases.
    return Path(os.path.relpath(path.resolve()))


def copy_tree_resolving_symlinks(src_dir: Path, dst_dir: Path):
    """Like shutil.copytree() but ensures that symlinks are fully resolved.

    When a source symlink points to another symlink, shutil.copytree()
    just creates another symlink at the destination. This function ensures
    that any symlink in the source tree results in a real file copy.

    Args:
       src_dir: Source directory path.
       dst_dir: Destination directory path.
    """
    for root, dirs, files in os.walk(src_dir):
        for file in files:
            src_file = Path(root) / file
            rel_file_path = src_file.relative_to(src_dir)

            if src_file.is_symlink():
                src_file = src_file.resolve()

            dst_file = dst_dir / rel_file_path
            dst_file.parent.mkdir(exist_ok=True, parents=True)
            shutil.copy2(src_file, dst_file)


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
    return subprocess.run([str(c) for c in cmd_args], cwd=cwd)


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
        "--target-cpus",
        nargs="+",
        required=True,
        help="List of target CPU names.")
    parser.add_argument("--stamp-file", help="Optional output stamp file.")
    parser.add_argument(
        "--fuchsia-dir", help="Specify Fuchsia source directory.")
    parser.add_argument(
        "--build-dir-name",
        default=_DEFAULT_BUILD_DIR_NAME,
        help=
        "Specify intermediate build directory prefix (default {_DEFAULT_BUILD_DIR_NAME})",
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

    build_dir_name = args.build_dir_name
    if not build_dir_name:
        parser.error("--build-dir-name value cannot be empty!")

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

    # This script uses a single common build directory, but switches the
    # content of args.gn between cpu-specific builds. This is done to avoid
    # rebuilding host binaries.
    #
    # Generating an exported sdk() target populates a directory like:
    #
    #   $BUILD_DIR/sdk/exported/<name>/
    #
    # With a tree of symlinks to build artifacts that are in other parts
    # of $BUILD_DIR/
    #
    # This script will copy these into:
    #
    #   $BUILD_DIR/sdk/<cpu>-exported/<name>/
    #
    # While following all symlinks to their final destination, i.e. copying
    # the files there.
    #

    build_dir = Path(build_dir_name)
    build_dir.mkdir(exist_ok=True, parents=True)

    if args.clean and build_dir.exists():
        log(f"Cleaning build directory {build_dir}")
        run_command([ninja_path, "-C", build_dir, "-t", "clean"])

    # Parse --sdk-targets GN labels and record related information for each entry.
    SdkTargetInfo = collections.namedtuple(
        "SdkTargetInfo", "name ninja_target stamp_file, exported_dir")

    sdk_targets = []
    for target_label in args.sdk_targets:
        # Expected format is //<dir>:<name>
        path, colon, name = target_label.partition(":")
        if colon != ":" or not path.startswith("//"):
            parser.error(f"Invalid SDK target label: {target_label}")

        sdk_targets.append(
            SdkTargetInfo(
                name=name,
                ninja_target=path[2:] + ":" + name,
                stamp_file=build_dir / f"gen/sdk/{name}.exported",
                exported_dir=build_dir / f"sdk/exported/{name}",
            ))

    # Parse --cpu-targets and record related information for reach entry.
    CpuInfo = collections.namedtuple("CpuInfo", "name args_gn result_dir")

    cpu_infos: List[CpuInfo] = []
    for target_cpu in args.target_cpus:
        # Generate the args.gn files for all CPUs in advance.
        args_gn_content = _ARGS_GN_TEMPLATE.format(
            cpu=target_cpu,
            cxx_rbe_enable="true" if args.cxx_rbe_enable else "false",
            rust_rbe_enable="true" if args.rust_rbe_enable else "false",
            use_goma="true" if args.use_goma else "false",
        )
        if args.use_goma and args.goma_dir:
            args_gn_content += 'goma_dir = "%s"\n' % args.goma_dir

        if target_cpu == "x64":
            # Ensure linux-arm64 host binaries are also generated
            # Only needed for a single sub-build.
            args_gn_content += "sdk_cross_compile_host_tools = true\n"

        cpu_infos.append(
            CpuInfo(
                name=target_cpu,
                args_gn=args_gn_content,
                result_dir=build_dir / "sdk" / f"{target_cpu}-exported",
            ))

    # Generate the Ninja build plans. Do this for all cpus before building
    # anything to catch all GN errors as soon as possible.
    for cpu_info in cpu_infos:
        with open(build_dir / "args.gn", "w") as f:
            f.write(cpu_info.args_gn)

        log(f"Checking GN/Ninja build plan for target_cpu {cpu_info.name}")
        if run_checked_command([gn_path, "--root=%s" % fuchsia_dir.resolve(),
                                "gen", build_dir]):
            return 1

    # Normally, the IDK is built using a merged_sdk("core") target in
    # //sdk/BUILD.gn. This takes a list of dependencies to sdk() targets, which
    # all create an independent $BUILD_DIR/sdk/exported/<name>/ directory,
    # then their content is merged into $BUILD_DIR/sdk/exported/core/ and
    # $BUILD_DIR/sdk/archive/core.tar.gz
    #
    # There is no point here in building merged_sdk() targets, and creating an
    # archive that will never be used, so just build the sdk() dependencies
    # directly instead, which are called "IDK sub-targets" here.
    for cpu_info in cpu_infos:
        # Overwrite args.gn
        with open(build_dir / "args.gn", "w") as f:
            f.write(cpu_info.args_gn)

        log(f"Generating {cpu_info.name} IDK sub-targets in {build_dir}")
        for sdk in sdk_targets:
            stamp_file = sdk.stamp_file
            if stamp_file.exists():
                os.unlink(stamp_file)
                exported_dir = build_dir / sdk.exported_dir
                if exported_dir.exists():
                    shutil.rmtree(exported_dir)

        if not (build_dir / "build.ninja").exists():
            if run_checked_command([gn_path,
                                    "--root=%s" % fuchsia_dir.resolve(), "gen",
                                    build_dir]):
                return 1

        if run_checked_command([ninja_path, "-C", build_dir] +
                               [sdk.ninja_target for sdk in sdk_targets]):
            return 1

        dst_dir = cpu_info.result_dir
        log(f"Copying exported SDKs to {dst_dir}")
        if dst_dir.exists():
            shutil.rmtree(dst_dir)
        dst_dir.mkdir(parents=True)
        for sdk in sdk_targets:
            copy_tree_resolving_symlinks(sdk.exported_dir, dst_dir / sdk.name)

    # Merge everything into the final directory (or archive).
    merge_cmd_args = [
        sys.executable,
        "-S",
        fuchsia_dir / "scripts" / "sdk" / "merger" / "merge.py",
    ]
    for cpu_info in cpu_infos:
        for sdk in sdk_targets:
            merge_cmd_args += [
                "--input-directory", cpu_info.result_dir / sdk.name
            ]

    if args.output_dir:
        merge_cmd_args += ["--output-directory", args.output_dir]
    else:
        merge_cmd_args += ["--output-archive", args.output_archive]

    if run_checked_command(merge_cmd_args):
        return 1

    # Remove result directories.
    for cpu_info in cpu_infos:
        for sdk in sdk_targets:
            shutil.rmtree(cpu_info.result_dir / sdk.name)

    # Write stamp file if needed.
    if args.stamp_file:
        with open(args.stamp_file, "w") as f:
            f.write("")

    return 0


if __name__ == "__main__":
    sys.exit(main())
