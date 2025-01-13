#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Convert the binary Bazel workspace log to text format."""

import argparse
import os
import shlex
import subprocess
import sys
from typing import Optional

_SCRIPT_DIR = os.path.dirname(__file__)
_FUCHSIA_DIR = os.path.abspath(os.path.join(_SCRIPT_DIR, "..", "..", ".."))

sys.path.insert(0, _SCRIPT_DIR)
import workspace_utils


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


def find_fuchsia_dir(from_dir: Optional[str] = None) -> Optional[str]:
    """Return Fuchsia directory path.

    Args:
        from_dir: Optional starting directory for the search. If None,
            uses the current directory.

    Returns:
        Fuchsia directory path, or None if it could not be found.
    """
    if from_dir is None:
        from_dir = os.getcwd()
    while True:
        if os.path.exists(os.path.join(from_dir, ".jiri_root")):
            return from_dir
        new_dir = os.path.dirname(from_dir)
        if new_dir == from_dir:
            # The top-level path was reached.
            return None
        from_dir = new_dir


def find_ninja_build_dir(fuchsia_dir: str) -> Optional[str]:
    """Find Ninja build directory.

    Args:
        fuchsia_dir: Fuchsia directory path.

    Returns:
        The Ninja build directory, or None if it could not be determined,
        which happens if `fx set` was not called previously.
    """
    fx_build_dir = os.path.join(fuchsia_dir, ".fx-build-dir")
    if not os.path.exists(fx_build_dir):
        print("ERROR: Could not find %s" % fx_build_dir, file=sys.stderr)
        return None

    with open(fx_build_dir) as f:
        build_dir = f.read().strip()

    if not build_dir:
        print("ERROR: Empty .fx-build-dir!", file=sys.stderr)
        return None

    return os.path.join(fuchsia_dir, build_dir)


def find_default_log_file() -> Optional[str]:
    """Find the location of the default log file.

    Returns:
        Path to the default log file, or None if it could not be determined.
    """
    fuchsia_dir = find_fuchsia_dir()
    if not fuchsia_dir:
        return None

    build_dir = find_ninja_build_dir(fuchsia_dir)
    if not build_dir:
        return None

    top_dir = os.path.join(
        build_dir,
        workspace_utils.get_bazel_relative_topdir(fuchsia_dir, "main"),
    )
    log_file = os.path.join(top_dir, "logs", "workspace-events.log")
    return log_file


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--jre",
        help="Specify Java JRE used to run the parser, default uses the prebuilt Bazel JRE.",
    )
    parser.add_argument(
        "--log-parser-jar",
        help="Specify alternative location for parser .jar file.",
    )
    parser.add_argument(
        "--exclude_rule",
        action="append",
        default=[],
        help="Rule(s) to filter out while parsing.",
    )
    parser.add_argument(
        "--output_path",
        help="Output file location. If not used, output goes to stdout.",
    )
    parser.add_argument("--log_file", help="Input log file (auto-detected).")
    parser.add_argument(
        "--verbose", action="store_true", help="Enable verbose mode."
    )
    args = parser.parse_args()

    # Find the Java Runtime Environment to run the parser first.
    java_binary = os.path.join("bin", "java")
    if sys.platform.startswith("win"):
        java_binary += ".exe"

    def find_java_binary(jre_path: str) -> Optional[str]:
        path = os.path.join(jre_path, java_binary)
        return path if os.path.exists(path) else None

    if args.jre:
        java_launcher = find_java_binary(args.jre)
        if not java_launcher:
            parser.error("Invalid JRE path: " + args.jre)
            return 1
    else:
        # Auto-detect the prebuilt bazel JRE first
        prebuilt_bazel_jdk = os.path.join(
            _FUCHSIA_DIR,
            "prebuilt",
            "third_party",
            "bazel",
            get_host_tag(),
            "install_base",
            "embedded_tools",
            "jdk",
        )
        java_launcher = find_java_binary(prebuilt_bazel_jdk)
        if not java_launcher:
            print(
                "ERROR: Missing prebuilt Bazel JDK launcher, please use --jre=<DIR>: %s/%s"
                % (prebuilt_bazel_jdk, java_binary),
                file=sys.stderr,
            )
            return 1

    def verbose(msg):
        if args.verbose:
            print("DEBUG: " + msg, file=sys.stderr)

    # Find the parser JAR file now.
    if args.log_parser_jar:
        log_parser_jar = args.log_parser_jar
    else:
        log_parser_jar = os.path.join(
            _FUCHSIA_DIR,
            "prebuilt",
            "third_party",
            "bazel_workspacelogparser",
            "bazel_workspacelogparser.jar",
        )

    if not os.path.exists(log_parser_jar):
        parser.error("Missing parser file: " + log_parser_jar)

    verbose("Using jar file at: " + log_parser_jar)

    log_file = args.log_file
    if not args.log_file:
        log_file = find_default_log_file()
        if not log_file:
            print(
                "ERROR: Could not find default log file, please use --log_file=FILE",
                file=sys.stderr,
            )
            return 1

    verbose("Using log file at: " + log_file)

    cmd = [java_launcher, "-jar", log_parser_jar, "--log_path=" + log_file]
    cmd += ["--exclude_rule=" + rule for rule in args.exclude_rule]
    if args.output_path:
        cmd += ["--output_path=" + args.output_path]

    verbose("Running command: %s" % " ".join([shlex.quote(c) for c in cmd]))

    return subprocess.run(cmd).returncode


if __name__ == "__main__":
    sys.exit(main())
