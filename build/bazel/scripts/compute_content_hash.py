#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Generate a content hash file from a given source repository content.

By default, scan all files in the source path (file or directory) and hash
their content.

However, if the directory's comes from a CIPD archive, using --cipd-name=NAME
will use the .versions/NAME.cipd_version file, if present, to compute the hash,
which is dramatically faster. If the file is not present, the default behavior
is used as a fallback.
"""

import argparse
import hashlib
import os
import sys
from pathlib import Path
from typing import Sequence


def content_hash_for_files(paths: Sequence[Path | str]) -> str:
    """Compute a unique content hash from a list of files.

    Args:
        paths: A list of Path objects.
    Returns:
        A string holding a hexadecimal content hash value.
    """
    h = hashlib.new("md5")
    for p in paths:
        p = Path(p)
        h.update(b"|")
        if p.is_symlink():
            h.update(bytes(p.readlink()))
        else:
            h.update(p.read_bytes())
    return h.hexdigest()


def find_content_files(
    source_dir: Path | str, cipd_name=None, exclude_extensions=[]
) -> Sequence[Path]:
    """Return a list of content files to send for hashing from a source directory.

    By default, this scans all files from source_dir, removing any files whose
    suffixes match exclude_extensions from the result.

    However, if cipd_name is provided, this will first look under
    ${source_dir}/.versions/${cipd_name}.cipd_version to look for a CIPD version
    file. If present, this will be the only path returned in the result.

    Args:
        source_dir: Source installation directory path.
        cipd_name: Optional CIPD archive name (e.g. 'cpython3').
        exclude_extensions: Optional list of file suffixes to omit from
           the result (e.g. [".pyc"]).
    Returns:
        A list of Path values.
    """
    source_dir = Path(source_dir)
    if cipd_name:
        cipd_version_file = (
            source_dir / ".versions" / f"{cipd_name}.cipd_version"
        )
        if cipd_version_file.exists():
            return [cipd_version_file]

    result = []
    for root, dirs, files in os.walk(source_dir):
        result.extend(
            Path(os.path.join(root, file))
            for file in files
            if not file.endswith(tuple(exclude_extensions))
        )
    return result


def _depfile_quote(p: Path) -> str:
    """Quote a Path value for depfile output."""
    return str(p).replace("\\", "\\\\").replace(" ", "\\ ")


def main(commandline_args):
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument(
        "--cipd-name", help="Provide name for optional CIPD version file.."
    )
    parser.add_argument(
        "--exclude-suffix",
        action="append",
        default=[],
        help='Exclude directory entries with given suffix (e.g. ".pyc").\n'
        + "Can be used multiple times.",
    )
    parser.add_argument(
        "source_path", type=Path, help="Source file or directory path."
    )
    parser.add_argument(
        "--output", type=Path, help="Optional output file path."
    )
    parser.add_argument(
        "--depfile", type=Path, help="Optional Ninja depfile output file path."
    )
    args = parser.parse_args(commandline_args)

    if not args.source_path.exists():
        parser.error("Path does not exist: %s", args.source_path)

    if args.depfile and not args.output:
        parser.error("--depfile option requires --output.")

    if args.source_path.is_dir():
        files = find_content_files(
            args.source_path,
            cipd_name=args.cipd_name,
            exclude_extensions=args.exclude_suffix,
        )
    else:
        files = [args.source_path]

    content_hash = content_hash_for_files(files)
    if args.output:
        args.output.write_text(content_hash)
    else:
        print(content_hash)

    if args.depfile:
        args.depfile.write_text(
            "%s: \\\n  %s\n"
            % (args.output, " \\\n  ".join(_depfile_quote(p) for p in files))
        )

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
