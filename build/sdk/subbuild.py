#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Build an sdk_collection target for a particular target_cpu and api_level."""

import argparse
import collections
import json
import multiprocessing
import os
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Set, Tuple

_ARGS_GN_TEMPLATE = r"""# Auto-generated - DO NOT EDIT
target_cpu = "{cpu}"
build_info_board = "{cpu}"
build_info_product = "bringup"
is_debug = false

cxx_rbe_enable = {cxx_rbe_enable}
rust_rbe_enable = {rust_rbe_enable}
use_goma = {use_goma}
universe_package_labels = [{sdk_labels_list}]
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


def command_args_to_string(args: List, env=None) -> str:
    elements = []
    if env:
        elements += [
            "%s=%s" % (name, shlex.quote(value))
            for name, value in sorted(env.items())
        ]
    elements += [shlex.quote(str(a)) for a in args]
    return " ".join(elements)


def run_command(args: List, cwd=None, env=None):
    """Run a command.

    Args:
        args: A list of strings or Path items (each one of them will be
            converted to a string for convenience).
        cwd: If not None, path to the directory where to run the command.
        env: If not None, a dictionary of environment variables to use.
    Returns:
        a subprocess.run() result value.
    """
    log("RUN: " + command_args_to_string(args, env))
    start_time = time.time()
    if env is not None:
        new_vars = env
        env = os.environ.copy()
        for name, value in new_vars.items():
            env[name] = value
    result = subprocess.run([str(a) for a in args], cwd=cwd, env=env)
    end_time = time.time()
    log("DURATION: %.1fs" % (end_time - start_time))
    return result


def run_checked_command(args: List, cwd=None, env=None):
    """Run a command, return True if succeeds, False otherwise.

    Args:
        args: A list of strings or Path items (each one of them will be
            converted to a string for convenience).
        cwd: If not None, path to the directory where to run the command.
        env: If not None, a dictionary of environment variables to use.
    Returns:
        True on success. In case of failure, print the command line and return False.
    """
    try:
        ret = run_command(args, cwd, env)
        if ret.returncode == 0:
            return False
    except KeyboardInterrupt:
        # If the user interrupts a long-running command, do not print anything.
        return True

    args_str = command_args_to_string(args, env)
    print(f"ERROR: When running command: {args_str}\n", file=sys.stderr)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-id", help="The version name value for this IDK.")
    parser.add_argument(
        "--sdk-collection-label",
        required=True,
        help="List of GN sdk_collection() GN targets to build.",
    )
    parser.add_argument(
        "--prebuilt-host-tools-dir",
        required=True,
        help="Build directory containing host tools.",
    )
    parser.add_argument(
        "--target-cpu",
        required=True,
        help="Target CPU for which to build the sdk_collection.",
    )
    parser.add_argument(
        "--api-level",
        type=int,
        required=True,
        help="API level at which to build the sdk_collection, or 0 to use the default.",
    )
    parser.add_argument("--stamp-file", help="Optional output stamp file.")
    parser.add_argument(
        "--fuchsia-dir", required=True, help="Fuchsia source directory."
    )
    parser.add_argument(
        "--output-build-dir",
        required=True,
        help="Build dir for the subbuild",
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
        "--use-goma", action="store_true", help="Whether to use goma or not."
    )
    parser.add_argument(
        "--goma-dir",
        help="Optional goma directory path, only used with --use-goma.",
    )
    parser.add_argument(
        "--parallelism",
        type=int,
        default=multiprocessing.cpu_count(),
        help="Parallelism argument (-j) to pass to ninja.",
    )
    parser.add_argument(
        "--max-load-average",
        type=int,
        default=multiprocessing.cpu_count(),
        help="Max load average argument (-l) to pass to ninja.",
    )
    parser.add_argument(
        "--compress-debuginfo",
        help="Optional value to select compression of debug sections in ELF binaries.",
    )
    parser.add_argument(
        "--host-tag", help="Fuchsia host os/cpu tag used to find prebuilts."
    )
    parser.add_argument(
        "--clean", action="store_true", help="Force clean build."
    )

    args = parser.parse_args()

    fuchsia_dir = Path(args.fuchsia_dir)

    # Locate GN and Ninja prebuilts.
    if args.host_tag:
        host_tag = args.host_tag
    else:
        host_tag = get_host_tag()

    gn_path = fuchsia_dir / "prebuilt" / "third_party" / "gn" / host_tag / "gn"
    if not gn_path.exists():
        return error(f"Missing gn prebuilt binary: {gn_path}")

    ninja_path = (
        fuchsia_dir / "prebuilt" / "third_party" / "ninja" / host_tag / "ninja"
    )
    if not ninja_path.exists():
        return error(f"Missing ninja prebuilt binary: {ninja_path}")

    def sdk_label_partition(target_label: str) -> Tuple[str, str]:
        """Split an SDK GN label into a (dir, name) pair."""
        # Expected format is //<dir>:<name>
        path, colon, name = target_label.partition(":")
        assert colon == ":" and path.startswith(
            "//"
        ), f"Invalid SDK target label: {target_label}"
        return (path[2:], name)

    def sdk_label_to_ninja_target(target_label: str) -> str:
        """Convert SDK GN label to Ninja target path."""
        target_dir, target_name = sdk_label_partition(target_label)
        return f"{target_dir}:{target_name}"

    def sdk_label_to_exported_dir(target_label: str, build_dir: Path) -> Path:
        """Convert SDK GN label to exported directory in build_dir."""
        target_dir, target_name = sdk_label_partition(target_label)
        return build_dir / "sdk" / "exported" / target_name

    target_cpu = args.target_cpu
    api_level = args.api_level

    build_dir = Path(args.output_build_dir)
    build_dir.mkdir(exist_ok=True, parents=True)
    log(f"{build_dir}: Preparing sub-build, directory: {build_dir.resolve()}")

    if args.clean and build_dir.exists():
        log(f"{build_dir}: Cleaning build directory")
        run_command([ninja_path, "-C", build_dir, "-t", "clean"])

    args_gn_content = _ARGS_GN_TEMPLATE.format(
        cpu=target_cpu,
        cxx_rbe_enable="true" if args.cxx_rbe_enable else "false",
        rust_rbe_enable="true" if args.rust_rbe_enable else "false",
        use_goma="true" if args.use_goma else "false",
        sdk_labels_list=f'"{args.sdk_collection_label}"',
    )
    if args.use_goma and args.goma_dir:
        args_gn_content += f'goma_dir = "{args.goma_dir}"\n'
    if args.sdk_id:
        args_gn_content += f'sdk_id = "{args.sdk_id}"\n'

    if args.compress_debuginfo:
        args_gn_content += f'compress_debuginfo = "{args.compress_debuginfo}"\n'

    # Reuse host tools from the top-level build. This assumes that
    # sub-builds cannot use host tools that were not already built by
    # the top-level build, as there is no way to inject dependencies
    # between the two build graphs.
    args_gn_content += (
        f'host_tools_base_path_override = "{args.prebuilt_host_tools_dir}"\n'
    )

    args_gn_content += "sdk_inside_sub_build = true\n"

    if api_level == 0:
        # The host architecture is built at the default level as part of the
        # main build, so only sub-builds for other target CPU architectures
        # should reach here.
        assert f"{target_cpu}" != f"{get_host_arch()}"
    else:
        # A non-default API level was specified.
        args_gn_content += "sdk_inside_supported_api_sub_build = true\n"
        args_gn_content += f"override_target_api_level = {api_level}\n"

    log(f"{build_dir}: args.gn content:\n{args_gn_content}")
    if (
        write_file_if_unchanged(build_dir / "args.gn", args_gn_content)
        or not (build_dir / "build.ninja").exists()
    ):
        if run_checked_command(
            [
                gn_path,
                "--root=%s" % fuchsia_dir.resolve(),
                "--root-pattern=//:developer_universe_packages",
                "gen",
                build_dir,
            ]
        ):
            return 1

    # Touch the sentinel file in the build directory to force any non-hermetic
    # build rules to rebuild.
    (build_dir / "force_nonhermetic_rebuild").touch()

    # Adjust the NINJA_STATUS environment variable before launching Ninja
    # in order to add a prefix distinguishing its build actions from
    # the top-level ones.
    ninja_status = os.environ.get("NINJA_STATUS", "[%f/%t](%r) ")

    if api_level == 0:
        status_prefix = f"IDK_SUBBUILD_{target_cpu} "
    else:
        status_prefix = f"IDK_SUBBUILD_api{api_level}-{target_cpu} "
    ninja_status = status_prefix + ninja_status
    ninja_env = {"NINJA_STATUS": ninja_status}

    if run_checked_command(
        [
            ninja_path,
            "-C",
            build_dir,
            "-j",
            args.parallelism,
            "-l",
            args.max_load_average,
            sdk_label_to_ninja_target(args.sdk_collection_label),
        ],
        env=ninja_env,
    ):
        return 1

    base_exported_dir = sdk_label_to_exported_dir(
        args.sdk_collection_label, build_dir
    )
    api_level_path = base_exported_dir / "api_level"
    api_level_path.write_text(str(api_level))

    # Write stamp file if needed.
    if args.stamp_file:
        with open(args.stamp_file, "w") as f:
            f.write("")

    return 0


if __name__ == "__main__":
    sys.exit(main())
