#!/usr/bin/env python3

# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Generate an OOT workspace that can be used for testing
things in-tree as if it lives in an OOT repository.
"""

import argparse
import sys
import shutil
import os
from pathlib import Path


def copy_file(src, dest, rename=""):
    if rename:
        dest = os.path.join(os.path.dirname(dest), rename)

    if os.path.exists(dest) or os.path.lexists(dest):
        return [src]

    parent = os.path.dirname(dest)
    if not os.path.exists(parent):
        os.makedirs(parent)

    shutil.copyfile(src, dest, follow_symlinks=False)

    if dest.endswith((".py", ".sh", "bazel")) and os.path.exists(dest):
        os.chmod(dest, 0o755)

    return [src]


def copy_dir(src, dest, rename=""):
    if not os.path.exists(dest):
        os.makedirs(dest)

    for dirpath, dirnames, filenames in os.walk(src, followlinks=True):
        for filename in filenames:
            in_file = os.path.join(dirpath, filename)

            rel_path = os.path.join(
                dirpath.removeprefix(os.path.dirname(src) + "/"),
                filename,
            )
            out_file = os.path.join(dest, rel_path)

            if rename:
                p = Path(rel_path)
                out_file = os.path.join(dest, rename, *p.parts[1:])

            copy_file(in_file, out_file)


def symlink(src, dest: str, target_is_directory=False):
    if not os.path.exists(src) and not os.path.islink(src):
        os.symlink(dest, src, target_is_directory=target_is_directory)
    return [src]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bazel-launcher", required=True)
    parser.add_argument(
        "--clang-prebuilt-dir",
        required=True,
    )
    parser.add_argument(
        "--sdk-dir",
        required=True,
    )
    parser.add_argument(
        "--googletest-dir",
        required=True,
    )
    parser.add_argument(
        "--fuchsia-infra-dir",
        required=True,
    )
    parser.add_argument(
        "--template-workspace-dir",
        required=True,
    )
    parser.add_argument(
        "--out-dir",
        required=True,
    )
    args = parser.parse_args()

    workspace_name = os.path.basename(args.template_workspace_dir)
    third_party_dir = os.path.join(args.out_dir, workspace_name, "third_party")
    scripts_dir = os.path.join(args.out_dir, workspace_name, "scripts")
    tools_dir = os.path.join(args.out_dir, workspace_name, "tools")
    bazel_binary = os.path.join(tools_dir, "bazel")
    clang_dir = os.path.join(third_party_dir, "fuchsia_clang")

    copy_dir(args.template_workspace_dir, args.out_dir)
    copy_dir(
        args.fuchsia_infra_dir,
        third_party_dir,
        rename="fuchsia-infra-bazel-rules",
    )
    copy_dir(args.clang_prebuilt_dir, third_party_dir, rename="fuchsia_clang")
    copy_dir(args.googletest_dir, third_party_dir, rename="googletest")
    copy_file(args.bazel_launcher, bazel_binary)

    # Symlink the Fuchsia SDK
    src = os.path.join(third_party_dir, "fuchsia_sdk")
    dest = args.sdk_dir
    if not os.path.exists(src):
        os.symlink(dest, src, target_is_directory=True)

    # tricium complains about dangling symlinks.
    # Instead of committing these to fuchsia.git, generate them now.
    symlink(
        os.path.join(scripts_dir, "bootstrap.sh"),
        "../third_party/fuchsia-infra-bazel-rules/scripts/bootstrap.sh",
    )
    symlink(
        os.path.join(tools_dir, "ffx"),
        "../third_party/fuchsia-infra-bazel-rules/scripts/run_sdk_tool.sh",
    )
    symlink(
        os.path.join(tools_dir, "fssh"),
        "../third_party/fuchsia-infra-bazel-rules/scripts/run_sdk_tool.sh",
    )

    return 0


if __name__ == "__main__":
    sys.exit(main())
