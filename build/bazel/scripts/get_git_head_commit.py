#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Extract the git HEAD commit value from a given git source directory.

This also supports creating a Ninja depfile to track the files that
participate in its computation.
"""

import argparse
import subprocess
import sys
import typing as T
from pathlib import Path


def find_git_head_inputs(repo_dir: Path) -> T.Set[Path]:
    """Find all input files used to determine the git HEAD of a given repository.

    Args:
        repo_dir: Path to a non-bare git repository directory.
    Returns:
        A set of Path values that can be used as implicit inputs in a depfile.
    """
    result: T.Set[Path] = set()

    git_dir = repo_dir / ".git"
    if git_dir.is_dir():
        # A regular .git sub-directory.
        pass
    elif git_dir.is_file():
        # A sub-module redirection file, contains 'gitdir: <path_to_real_git_dir>'
        result.add(git_dir)
        content = git_dir.read_text().strip()
        assert content.startswith("gitdir: ")
        git_dir = repo_dir / content[8:]

    # The .git/config file will be accessed if it exists.
    git_config = git_dir / "config"
    if git_config.exists():
        result.add(git_config)

    # The .git/packed-refs file will be accessed if it exists,
    # even in detached state.
    git_packed_refs = git_dir / "packed-refs"
    if git_packed_refs.exists():
        result.add(git_packed_refs)

    git_head = git_dir / "HEAD"
    result.add(git_head)

    if git_head.exists():
        # HEAD can be an hexadecimal commit value (detached state)
        # or "ref: refs/heads/<branch>" (when on a branch).
        content = git_head.read_text().strip()
        if content.startswith("ref: "):
            branch_ref = git_dir / content[5:]

            # refs/heads/<branch> might not exist when packed references
            # are enabled, in this case, the value is in .git/packed-refs
            # instead.
            if branch_ref.exists():
                result.add(branch_ref)

    return result


def get_git_head_commit(repo_dir: Path, git_binary: Path = Path("git")) -> str:
    """Return the git HEAD commit value of a given repository.

    Args:
        repo_dir: Path to a git repository directory.
        git_binary: Optional path to the git binary to use, default to "git".
    Returns:
        An hexadecimal string value for the HEAD commit (even when on a branch).
        This will be "GIT_ERROR" if an error happened when trying to get it.
    """
    ret = subprocess.run(
        [git_binary, "-C", repo_dir, "rev-parse", "HEAD"],
        text=True,
        capture_output=True,
    )
    if ret.returncode != 0:
        return "GIT_ERROR"

    return ret.stdout.strip()


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument(
        "--git",
        type=Path,
        help="Specify path to git binary (auto-detected from PATH)",
    )
    parser.add_argument(
        "--output", type=Path, help="Optional output file path."
    )
    parser.add_argument(
        "--depfile", type=Path, help="Optional Ninja depfile output file path."
    )
    parser.add_argument(
        "repo_dir", type=Path, help="Path to git repository directory."
    )
    args = parser.parse_args()

    if args.depfile and not args.output:
        parser.error("--depfile option requires --output.")

    result = get_git_head_commit(args.repo_dir, args.git)
    if args.output:
        args.output.write_text(result)
    else:
        print(result)

    if args.depfile:
        depfile_inputs = find_git_head_inputs(args.repo_dir)
        args.depfile.write_text(
            "%s: %s\n"
            % (args.output, " ".join(str(f) for f in sorted(depfile_inputs)))
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
