#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
""" This script checks python type checking for the input sources.
"""
import argparse
import subprocess
import sys
import os
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--sources",
        help="Sources of this target, including main source",
        nargs="*",
    )
    parser.add_argument(
        "--output",
        help="Path to output file",
    )
    parser.add_argument(
        "--library_infos",
        help="Path to the library infos JSON file",
        type=argparse.FileType("r"),
        required=True,
    )
    args = parser.parse_args()

    # To satisfy GN action requirements, creating a necessary empty output file
    Path(args.output).touch()

    lib_infos = json.load(args.library_infos)
    return run_mypy_checks(args.sources, lib_infos)


def run_mypy_checks(
    src_files: list[str], lib_infos: list[dict[str, str]]
) -> int:
    """
    Runs `mypy` type checking on the provided file paths and library sources
    that have "mypy_enable = True"', excluding duplicates.

    Args:
        src_files: List of source file paths to run type checking on
        lib_infos: List of library infos

    Returns:
        int: returncode if type checking was successful, else error returncode
    """

    lib_files = get_mypy_enabled_library_sources(lib_infos)
    # Remove the duplicate and non-MyPy supported files
    files = exclude_files(src_files + lib_files)
    if not files:
        return ""

    fuchsia_dir = Path(__file__).parent.parent.parent
    config_path = fuchsia_dir / "pyproject.toml"
    pylibs_dir = fuchsia_dir / "third_party" / "pylibs"
    try:
        return subprocess.run(
            [
                sys.executable,
                "-S",
                "-m",
                "mypy",
                "--config-file",
                str(config_path),
            ]
            + files,
            env={
                "PYTHONPATH": os.pathsep.join(
                    [
                        str(pylibs_dir / "mypy" / "src"),
                        str(pylibs_dir / "mypy_extensions" / "src"),
                        str(pylibs_dir / "typing_extensions" / "src" / "src"),
                    ]
                )
            },
            capture_output=True,
            text=True,
            check=True,
        ).returncode
    except subprocess.CalledProcessError as e:
        if e.returncode != 0:
            if e.stdout:
                print(
                    f"\nPlease fix the following Mypy errors:\n{e.stdout}\n",
                    file=sys.stderr,
                )
            else:
                print(
                    f"\nError occured during Mypy type checking:\n{e.stderr}\n",
                    file=sys.stderr,
                )
        return e.returncode


def get_mypy_enabled_library_sources(
    lib_infos: list[dict[str, str]]
) -> list[str]:
    """Returns a list of library sources with MyPy type checking enabled targets.

    Args:
        lib_infos: List of library infos

    Returns:
        list of MyPy type checking enabled library sources
    """
    type_check_files = []
    for info in lib_infos:
        # Add the target sources only if type checking support is enabled.
        type_check_files += [
            os.path.join(info["source_root"], source)
            for source in info["sources"]
            if info["mypy_support"]
        ]
    return type_check_files


def exclude_files(file_list: list[str]) -> set[str]:
    """Returns a list of unique files with Mypy-supported file extensions.

    Args:
        file_list: List of file paths

    Returns:
        set of Mypy-supported file paths
    """
    supported_extensions = (".py", ".pyi", ".pyx")
    return sorted(
        set(
            file
            for file in file_list
            if os.path.splitext(file)[1].lower() in supported_extensions
        )
    )


if __name__ == "__main__":
    sys.exit(main())
