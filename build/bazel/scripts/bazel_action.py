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
import subprocess
import sys

_SCRIPT_DIR = os.path.dirname(__file__)

# NOTE: Assume this script is located under build/bazel/scripts/
_FUCHSIA_DIR = os.path.abspath(os.path.join(_SCRIPT_DIR, '..', '..', '..'))

# A list of built-in Bazel workspaces like @bazel_tools// which are actually
# stored in the prebuilt Bazel install_base directory with a timestamp *far* in
# the future, e.g. 2033-01-01. This is a hack that Bazel uses to determine when
# its install base has changed unexpectedly.
#
# If any file from these directories are listed in a depfile, they will force
# rebuilds on *every* Ninja invocations, because the tool will consider that
# the outputs are always older (as 2022 < 2033).
#
# This list is thus used to remove these from depfile inputs. Given that the
# files are part of Bazel's installation, their content would hardly change
# between build invocations anyway, so this is safe.
#
_BAZEL_BUILTIN_REPOSITORIES = ('@bazel_tools//',)

# See long technical note at the end of this script to understand why
# these build files must be ignored in depfiles. Short version: this prevents
# incremental build breaks.
_BAZEL_IGNORED_BUILD_LABELS = {'@legacy_ninja_build_outputs//:BUILD.bazel'}

# Set this to True to debug operations in this script.
_DEBUG = False


def debug(msg):
    # Print debug message to stderr if _DEBUG is True.
    if _DEBUG:
        print('DEBUG: ' + msg, file=sys.stderr)


def copy_file_if_changed(src_path, dst_path):
    # NOTE: For some reason, filecmp.cmp() will return True if
    # dst_path does not exist, even if src_path is not empty!?
    if os.path.exists(dst_path) and filecmp.cmp(src_path, dst_path,
                                                shallow=False):
        return

    # Use lexists to make sure broken symlinks are removed as well.
    if os.path.lexists(dst_path):
        os.remove(dst_path)
    try:
        os.link(src_path, dst_path)
    except OSError as e:
        if e.errno == errno.EXDEV:
            # Cross-device link, simple copy.
            shutil.copy2(src_path, dst_path)
        else:
            raise


class BazelLabelMapper(object):
    """Provides a way to map Bazel labels to file paths.

    Usage is:
      1) Create instance, passing the path to the Bazel workspace.
      2) Call source_label_to_path(<label>) where the label comes from
         a query.
    """

    def __init__(self, bazel_workspace):
        # Get the $OUTPUT_BASE/external directory from the $WORKSPACE_DIR,
        # the following only works in the context of the Fuchsia platform build
        # because the workspace/ and output_base/ directories are always
        # parallel entries in the $BAZEL_TOPDIR.
        #
        # Another way to get it is to call `bazel info output_base` and append
        # /external to the result, but this would slow down every call to this
        # script, and is not worth it for now.
        #
        self._root_workspace = os.path.abspath(bazel_workspace)
        output_base = os.path.normpath(
            os.path.join(bazel_workspace, '..', 'output_base'))
        assert os.path.isdir(output_base), f'Missing directory: {output_base}'
        self._external_dir = os.path.join(output_base, 'external')

    def source_label_to_path(self, label, relative_to=None):
        """Convert a Bazel label to a source file into the corresponding file path.

        Args:
          label: A fully formed Bazel label, as return by a query. If BzlMod is
              enabled, this expects canonical repository names to be present
              (e.g. '@foo.12//src/lib:foo.cc' and no '@foo//src/lib:foo.cc').
          relative_to: Optional directory path string.
        Returns:
          If relative_to is None, the absolute path to the corresponding source
          file, otherwise, the same path relative to `relative_to`.
        """
        #
        # NOTE: Only the following input label formats are supported
        #
        #    //<package>:<target>
        #    @//<package>:<target>
        #    @<name>//<package>:<target>
        #    @@<name>.<version>//<package>:<target>
        #
        repository, sep, package_label = label.partition('//')
        assert sep == '//', f'Missing // in source label: {label}'
        if repository == '' or repository == '@':
            # @// references the root project workspace, it should normally
            # not appear in queries, but handle it here just in case.
            #
            # // references a path relative to the current workspace, but the
            # queries are always performed from the root project workspace, so
            # is equivalent to @// for this function.
            repository_dir = self._root_workspace
        else:
            # A note on canonical repository directory names.
            #
            # An external repository named 'foo' in the project's WORKSPACE.bazel
            # file will be stored under `$OUTPUT_BASE/external/foo` when BzlMod
            # is not enabled.
            #
            # However, it will be stored under `$OUTPUT_BASE/external/@foo.<version>`
            # instead when BzlMod is enabled, where <version> is determined statically
            # by Bazel at startup after resolving the dependencies expressed from
            # the project's MODULE.bazel file.
            #
            # It is not possible to guess <version> here but queries will always
            # return labels for items in the repository that look like:
            #
            #   @@foo.<version>//...
            #
            # This is called a "canonical label", this allows the project to use
            # @foo to reference the repository in its own BUILD.bazel files, while
            # a dependency module would call it @com_acme_foo instead. All three
            # labels will point to the same location.
            #
            # Queries always return canonical labels, so removing the initial @
            # and the trailing // allows us to get the correct repository directory
            # in all cases.
            assert repository.startswith('@'), (
                f'Invalid repository name in source label {label}')
            repository_dir = os.path.join(self._external_dir, repository[1:])

        package, colon, target = package_label.partition(':')
        assert colon == ':', f'Missing colon in source label {label}'
        path = os.path.join(repository_dir, package, target)
        if relative_to:
            path = os.path.relpath(path, relative_to)
        return path


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
    parser.add_argument(
        '--fuchsia-dir',
        help='Path to Fuchsia source directory, auto-detected.')
    parser.add_argument('--depfile', help='Ninja depfile output path.')
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

    if args.depfile:
        # Perform a cquery to get all source inputs for the targets, this
        # returns a list of Bazel labesl followed by "(null)" because these
        # are never configured. E.g.:
        #
        #  //build/bazel/examples/hello_world:hello_world (null)
        #
        if args.fuchsia_dir:
            fuchsia_dir = os.path.abspath(args.fuchsia_dir)
        else:
            fuchsia_dir = _FUCHSIA_DIR

        source_dir = os.path.relpath(fuchsia_dir, os.getcwd())

        mapper = BazelLabelMapper(args.workspace_dir)

        # All bazel targets as a set() expression for Bazel queries below.
        # See https://bazel.build/query/language#set
        query_targets = 'set(%s)' % ' '.join(args.bazel_targets)

        # This query lists all BUILD.bazel and .bzl files, because the
        # buildfiles() operator cannot be used in the cquery below.
        #
        # --config=quiet is used to remove Bazel verbose output.
        #
        # --noimplicit_deps removes 11,000 files from the result corresponding
        # to the C++ and Python prebuilt toolchains
        #
        # --output label ensures the output contains one label per line,
        # which will be followed by '(null)' for source files (or more
        # generally by a build configuration name or hex value for non-source
        # targets which should not be returned by this cquery).
        #
        # --keep_going is necessary to avoid a query error because the
        #   build files reference through select() statements labels to targets
        #   that do not exist, for example `@fuchsia_sdk//pkg/fdio/x64` while the build
        #   configuration only supports arm64). The error looks like:
        #
        #   ```
        #   ERROR: .../external/fuchsia_sdk/pkg/fdio/BUILD.bazel:20:11: no such package '@fuchsia_sdk//pkg/fdio/x64':
        #          BUILD file not found in directory 'pkg/fdio/x64' of external repository @fuchsia_sdk. Add a BUILD file
        #          to a directory to mark it as a package. and referenced by '@fuchsia_sdk//pkg/fdio:fdio'
        #   ```
        query_cmd = [
            args.bazel_launcher,
            'query',
            '--config=quiet',
            '--noimplicit_deps',
            '--output',
            'label',
            '--keep_going',
            f'buildfiles(deps({query_targets}))',
        ]

        debug('QUERY_CMD: %s' % ' '.join(shlex.quote(c) for c in query_cmd))
        ret = subprocess.run(query_cmd, capture_output=True, text=True)
        # The subprocess always return an error for this specific query
        # (see comment above for --keep_going), so ignore all error messages
        # from stderr and the status code, except when _DEBUG is set, where
        # they will be printed for human verification. Using _DEBUG directly
        # prevents un-needed string join() operations in ret.stderr content
        # in release mode.
        if _DEBUG and ret.returncode:
            debug(
                'Error when calling Bazel query: %s\n%s\n' %
                (' '.join(shlex.quote(c) for c in query_cmd), ret.stderr))

        # A function that returns True if the label of a BUILD.bazel or .bzl
        # file should be ignored.
        def is_ignored_build_file_label(label):
            debug(f'LABEL [{label}]')
            return label.startswith(_BAZEL_BUILTIN_REPOSITORIES) or \
                   label in _BAZEL_IGNORED_BUILD_LABELS

        all_inputs = [
            label for label in ret.stdout.splitlines()
            if not is_ignored_build_file_label(label)
        ]

        # This cquery lists all source files. The output is one label per line
        # which will be followed by '(null)' for source files.
        #
        # (More generally this would be a build configuration name or hex
        # value for non-source targets which should not be returned by this
        # cquery).
        #
        cquery_cmd = [
            args.bazel_launcher,
            'cquery',
            '--config=quiet',
            '--noimplicit_deps',
            '--output',
            'label',
            f'kind("source file", deps({query_targets}))',
        ]

        debug('CQUERY_CMD: %s' % ' '.join(shlex.quote(c) for c in cquery_cmd))
        ret = subprocess.run(cquery_cmd, capture_output=True, text=True)
        if ret.returncode != 0:
            print(
                'WARNING: Error when calling Bazel cquery: %s\n%s\n%s\n' % (
                    ' '.join(shlex.quote(c)
                             for c in cquery_cmd), ret.stderr, ret.stdout),
                file=sys.stderr)

        for line in ret.stdout.splitlines():
            label, _, config = line.partition(' ')
            assert config == '(null)', f'Invalid cquery output: {line} (config {config})'

            if label.startswith(_BAZEL_BUILTIN_REPOSITORIES):
                continue

            all_inputs.append(label)

        debug('ALL INPUTS:\n%s' % '\n'.join(all_inputs))
        # Convert output labels to paths relative to the current directory.
        all_sources = [
            mapper.source_label_to_path(label, relative_to=os.getcwd())
            for label in all_inputs
        ]

        depfile_content = '%s: %s\n' % (
            ' '.join(shlex.quote(c) for c in args.ninja_outputs), ' '.join(
                shlex.quote(c) for c in all_sources))

        debug('DEPFILE[%s]\n' % depfile_content)
        with open(args.depfile, 'w') as f:
            f.write(depfile_content)

    # Done!
    return 0


if __name__ == '__main__':
    sys.exit(main())

# Technical note on @legacy_ninja_build_outputs//:BUILD.bazel
#
# This file (a.k.a. LEGACY_BUILD) is generated by a Bazel repository rule the
# first time a `bazel build ...` command is performed after the Bazel workspace
# was created or updated, by reading and processing the content of
# $BAZEL_TOPDIR/legacy_ninja_build_outputs.inputs_manifest.json,
# a.k.a. INPUTS_MANIFEST.
#
# This INPUTS_MANIFEST file is very special because:
#
# - It is written at `gn gen` time, through the definition of the
#   //build/bazel:legacy_ninja_build_outputs target, which is very special:
#   it is testonly, and *nothing* else should depend on it in the GN graph.
#
# - It does not appear in the Ninja build plan as either an input or
#   output path, even though it _does_ appear in the command lines of
#   Ninja edges that implement GN bazel_action() targets (which end up
#   calling this script).
#
# Bazel build commands will regenerate LEGACY_BUILD file file every time they
# see that the INPUTS_MANIFEST file has changed.
#
# The LEGACY_BUILD file is also returned in the `bazel query ...` command
# performed above to generate the list of all input BUILD.bazel or .bzl files
# that go into the depfile for the action's command.
#
# This leads to a problematic situation in case of incremental builds
# that triggers un-wanted rebuilds (instead of no-ops). Here is an example:
#
# 1) A clean Fuchsia checkout is created, and `fx gen` is called, which
#    creates the INPUTS_MANIFEST file, but not the Bazel workspace yet.
#
# 2) `fx build` is called for the first time to build everything. There is
#    no Ninja deps log, or Ninja build log yet. This works, but the
#    following happens:
#
#      a) A Ninja action invokes this script to build a given FOO_OUT_FILE,
#         which is one output of a given `bazel_action("foo")` target.
#
#      b) The script invokes `bazel build <bazel_targets>`
#
#      c) Bazel runs for the first time and sets up repositories, this
#         creates the LEGACY_BUILD file from the contents of INPUTS_MANIFEST.
#
#      d) Bazel continues to run and builds an output file (a.k.a.
#         BAZEL_FOO_OUT_FILE) located in the output_base directory, then exits.
#
#      e) The script copies BAZEL_FOO_OUT_FILE to the final FOO_OUT_FILE
#         destination, using copy_file_if_changed() above.
#
#      f) The script uses a `bazel query ...` to get the list of all
#         BUILD.bazel and .bzl files that were used as input to build
#         <bazel_targets>. This lists includes the LEGACY_BUILD.
#
#         All these files are added to the Ninja deps log, as _implicit_
#         inputs for FOO_OUT_FILE.
#
#    Repeat this for a `bazel_action("bar")` target, and corresponding
#    BAR_OUT_FILE and BAZEL_BAR_OUT_FILE.
#
#    In more graphical terms:
#
#    TIMESTAMPS
#           |
#       100 +-- INPUTS_MANIFEST created by `fx gen`.
#           |
#           |
#       101 +-- Ninja walks its build graph, and visits the node for "foo".
#           |   Since FOO_OUT_FILE does not exists, it decides to run the
#           |   command which invokes this script, which invokes
#           |   `bazel build <foo_bazel_targets>`.
#           |
#       102 +-- LEGACY_BUILD created by `bazel build <foo_bazel_targets>`.
#           |
#       103 +-- BAZEL_FOO_OUT_FILE created by `bazel build <foo_bazel_targets>`.
#           |
#       104 +-- FOO_OUT_FILE copied from BAZEL_FOO_OUT_FILE by script.
#           |
#       105 +-- Script uses `bazel query ...` and adds LEGACY_BUILD to the
#           |   depfile entry for FOO_OUT_FILE.
#           |
#       106 +-- Ninja continues its walk, and visits node for "bar".
#           |
#       107 +-- BAZEL_BAR_OUT_FILE is created by `bazel build <bar_bazel_targets>`.
#           |
#       108 +-- BAR_OUT_FILE is copied from BAZEL_BAR_OUT_FILE by script.
#           |
#       109 +-- LEGACY_BUILD added to depfile entry fot BAR_OUT_FILE
#           |
#           v
#
#    Note also that in this example:
#
#    - Ninja knows that FOO_OUT_FILE is an output for "foo".
#    - Ninja knows that BAR_OUT_FILE is an output for "bar".
#
#    - Ninja knows that LEGACY_BUILD is an _implicit_ input for "foo",
#      and also an implicit input for "bar".
#
#    - Ninja doesn't know about FOO_BAZEL_OUT_FILE or BAR_BAZEL_OUT_FILE at all.
#
#    - Ninja doesn't know about INPUTS_MANIFEST, even though it is a hidden
#      input to `bazel build` (and the only reason it is passed to this script
#      is for consistency checking and printing a human-friendly error message
#      if needed).
#
# 3) `fx build` is called a second time by infra script to verify that
#    there is no unexpected Ninja rebuild. This works as follows:
#
#    TIMESTAMPS
#           |
#       200 +-- Ninja visits node for "foo". Sees that FOO_OUT_FILE timestamp
#           |   is 104 which is newer than the one of LEGACY_BUILD (101) or
#           |   any other input. Does not re-run the node's action.
#           |
#       201 +-- Ninja visits node for "bar". Sees that BAR_OUT_FILE timestamp
#           |   (108) is newer than the one of LEGACY_BUILD (101) or any other
#           |   input. Does not re-run the node's action.
#           V
#
# 4) A `jiri update` operation updates the Fuchsia checkout. It changes
#    some GN build files that affect the content of INPUTS_MANIFEST, in
#    ways that change the inputs for "bar", but not the inputs for "foo" (!!)
#
# 5) `fx build` is called again. This invokes `fx gen` automatically which
#    generates a new version of INPUTS_MANIFEST, with a fresh timestamp
#    (e.g. 200). Then:
#
#    TIMESTAMPS
#           |
#       300 +-- `fx gen` is run automatically, and updates INPUTS_MANIFEST
#           |
#       301 +-- Ninja visits node for "foo". Sees that FOO_OUT_FILE timestamp
#           |   is 104 which is newer than the one of LEGACY_BUILD (101) or
#           |   any other input. Does not re-run the node's action.
#           |
#       302 +-- Ninja visits node for "bar". Sees that BAR_OUT_FILE timestamp
#           |   (108) is newer than the one of LEGACY_BUILD (101) but one of
#           |   its inputs has changed. Decides to re-run the action,
#           |
#       303 +-- This script is called for "bar", which invokes
#           |   `bazel build <bar_bazel_targets>`
#           |
#       304 +-- Bazel sees that INPUTS_MANIFEST was changed, and regenerates
#           |   LEGACY_BUILD, updating its timestamp to 304.
#           |
#       305 +-- Bazel builds a new BAR_BAZEL_OUT_FILE
#           |
#       306 +-- BAR_OUT_FILE is updated from BAR_BAZEL_OUT_FILE.
#           |
#           V
#
# 6) `fx build` is called again to verify Ninja no-ops, unfortunately:
#
#    TIMESTAMPS
#           |
#       400 +-- Ninja visits node for "foo". Sees that FOO_OUT_FILE timestamp
#           |   is 104 is now older than the one for LEGACY_BUILD (304), and
#           |   decides to re-run the node's action !!
#           |
#       401 +-- This script invokes `bazel build <foo_bazel_targets>`.
#           |
#       402 +-- Bazel sees that FOO_BAZEL_OUT_FILE doesn't need to be rebuilt
#           |   since none of its _source_ files was changed.
#           |
#       403 +-- copy_file_if_changed() sees that FOO_OUT_FILE doesn't need to
#           |   be updated, since its content is still the same as
#           |   FOO_BAZEL_OUT_FILE.
#           |
#           |   The Ninja action completes successfully, and the build completes
#           |   because the command did not update its output, which is normal
#           |   behavior.
#           V
#
#   Also note that the FOO_OUT_FILE timestamp was not updated, so the next
#   `fx build` will _again_ run the action, and not update any output!
#
# PROBLEM RESOLUTION
#
# This problem would not exist if FOO_OUT_FILE would declare INPUTS_MANIFEST
# as one of its inputs. In this case, operation 301 above would detect that
# INPUTS_MANIFEST changed, and would trigger the action properly.
#
# Alas, nothing can depend in the GN graph on the target that generates
# INPUTS_MANIFEST, and the reason to do so is to break the testonly barrier
# between Ninja and Bazel.
#
# A work-around it to ignore LEGACY_BUILD as an implicit input, i.e. never
# include its path in the depfile for any bazel_action() target.
#
