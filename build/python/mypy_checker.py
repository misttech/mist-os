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
from pathlib import Path

# Because files are directly passed, the mypy --exclude flag is bypassed. As a
# result, we maintain an exclusion list here instead of in
# `fuchsia/pyproject.toml`, and we use it to ignore files from the mypy command.
_EXCLUDE_PATTERNS = [
    "third_party/",
    "out/",
    "prebuilt/",
    "__pycache__",
    ".jiri_root",
]


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
    args = parser.parse_args()

    # To satisfy GN action requirements, creating a necessary empty output file
    Path(args.output).touch()

    return run_mypy_checks(args.sources)


def run_mypy_checks(files: list[str]) -> int:
    """
    Runs `mypy` type checking on the provided file paths, excluding duplicates

    Args:
        files: List of file paths to run type checking on.

    Returns:
        int: returncode if type checking was successful, else error returncode
    """

    # Remove the duplicate and excluded files.
    files = exclude_files(files)
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


def exclude_files(file_list: list[str]) -> set[str]:
    """Exclude files matching the exclude_patterns from the file_list and
        remove duplicate file paths

    Args:
        file_list: List of file paths

    Returns:
        set of file paths after excluding the specified patterns
    """
    return sorted(
        set(
            file
            for file in file_list
            if not any(pattern in file for pattern in _EXCLUDE_PATTERNS)
        )
    )


if __name__ == "__main__":
    sys.exit(main())
