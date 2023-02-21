#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"Run a Bazel command from Ninja. See bazel_action.gni for details."

import argparse
import errno
import filecmp
import json
import os
import shlex
import shutil
import stat
import subprocess
import sys


def copy_file_if_changed(src_path, dst_path):
    """Copy |src_path| to |dst_path| if they are different."""
    # NOTE: For some reason, filecmp.cmp() will return True if
    # dst_path does not exist, even if src_path is not empty!?
    if os.path.exists(dst_path) and filecmp.cmp(src_path, dst_path,
                                                shallow=False):
        return

    # Use lexists to make sure broken symlinks are removed as well.
    if os.path.lexists(dst_path):
        os.remove(dst_path)

    # See https://fxbug.dev/121003 for context.
    # If the file is writable, try to hard-link it directly. Otherwise,
    # or if hard-linking fails due to a cross-device link, do a simple
    # copy.
    do_copy = True
    file_mode = os.stat(src_path).st_mode
    is_src_readonly = file_mode & stat.S_IWUSR == 0
    if not is_src_readonly:
        try:
            os.link(src_path, dst_path)
            do_copy = False
        except OSError as e:
            if e.errno != errno.EXDEV:
                raise

    if do_copy:
        shutil.copy2(src_path, dst_path)
        if is_src_readonly:
            os.chmod(dst_path, file_mode | stat.S_IWUSR)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--bazel-launcher',
        required=True,
        help='Path to Bazel launcher script.')
    parser.add_argument(
        '--workspace-dir', required=True, help='Bazel workspace path')
    parser.add_argument(
        '--command',
        required=True,
        help='Bazel command, e.g. `build`, `run`, `test`')
    parser.add_argument(
        '--inputs-manifest',
        help=
        'Path to the manifest file describing Ninja outputs as bazel inputs.')
    parser.add_argument(
        '--bazel-inputs',
        default=[],
        nargs='*',
        help='Labels of GN bazel_input_xxx() targets for this action.')
    parser.add_argument(
        '--bazel-targets',
        action='append',
        default=[],
        help='List of bazel target patterns.')
    parser.add_argument(
        '--bazel-outputs',
        default=[],
        nargs='*',
        help='Bazel output paths, relative to bazel-bin/.')
    parser.add_argument(
        '--bazel-platform',
        help='Set the Bazel target/default platform for this action.')
    parser.add_argument(
        '--ninja-outputs',
        default=[],
        nargs='*',
        help='Ninja output paths relative to current directory.')
    parser.add_argument('extra_bazel_args', nargs=argparse.REMAINDER)

    args = parser.parse_args()

    if not args.bazel_targets:
        return parser.error('A least one --bazel-targets value is needed!')

    if not args.bazel_outputs:
        return parser.error('At least one --bazel-outputs value is needed!')

    if not args.ninja_outputs:
        return parser.error('At least one --ninja-outputs value is needed!')

    if len(args.bazel_outputs) != len(args.ninja_outputs):
        return parser.error(
            'The --bazel-outputs and --ninja-outputs lists must have the same size!'
        )

    if args.bazel_inputs:
        if not args.inputs_manifest:
            return parser.error(
                '--inputs-manifest is required with --bazel-inputs')

        # Verify that all bazel input labels are pare of the inputs manifest.
        # If not, print a user-friendly message that explains the situation and
        # how to fix it.
        with open(args.inputs_manifest) as f:
            inputs_manifest = json.load(f)

        all_input_labels = set(e['gn_label'] for e in inputs_manifest)
        unknown_labels = set(args.bazel_inputs) - all_input_labels
        if unknown_labels:
            print(
                '''ERROR: The following bazel_inputs labels are not listed in the Bazel inputs manifest:

  %s

These labels must be in one of `gn_labels_for_bazel_inputs` or `extra_gn_labels_for_bazel_inputs`.
For more details, see the comments in //build/bazel/legacy_ninja_build_outputs.gni.
''' % '\n  '.join(list(unknown_labels)),
                file=sys.stderr)
            return 1

    if args.extra_bazel_args and args.extra_bazel_args[0] != '--':
        return parser.error(
            'Extra bazel args should be seperate with script args using --')
    args.extra_bazel_args = args.extra_bazel_args[1:]

    if not os.path.exists(args.workspace_dir):
        return parser.error(
            'Workspace directory does not exist: %s' % args.workspace_dir)

    if not os.path.exists(args.bazel_launcher):
        return parser.error(
            'Bazel launcher does not exist: %s' % args.bazel_launcher)

    cmd = [args.bazel_launcher, args.command]

    if args.bazel_platform:
        cmd.append('--platforms=' + args.bazel_platform)

    cmd += [
        shlex.quote(arg) for arg in args.extra_bazel_args
    ] + args.bazel_targets

    ret = subprocess.run(cmd)
    if ret.returncode != 0:
        print(
            'ERROR when calling Bazel. To reproduce, run this in the Ninja output directory:\n\n  %s\n'
            % ' '.join(shlex.quote(c) for c in cmd),
            file=sys.stderr)
        return 1

    for bazel_out, ninja_out in zip(args.bazel_outputs, args.ninja_outputs):
        src_path = os.path.join(args.workspace_dir, bazel_out)
        dst_path = ninja_out
        copy_file_if_changed(src_path, dst_path)

    # Done!
    return 0


if __name__ == "__main__":
    sys.exit(main())
