#!/usr/bin/env python3
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"Run a Bazel command from Ninja. See bazel_action.gni for details."

import argparse
import dataclasses
import errno
import filecmp
import os
import shlex
import shutil
import stat
import subprocess
import sys
from typing import Any, Dict, Iterable, List, Optional, Set, Tuple, TypeAlias

# Directory where to find Starlark input files.
_STARLARK_DIR = os.path.join(os.path.dirname(__file__), "..", "starlark")

# Directory where to find templated files.
_TEMPLATE_DIR = os.path.join(os.path.dirname(__file__), "..", "templates")

# A type for the JSON-decoded content describing the @gn_targets repository.
GnTargetsManifest: TypeAlias = List[Dict[str, Any]]

# The name of the root Bazel workspace as it appears in its WORKSPACE.bazel file.
# LINT.IfChange
_BAZEL_ROOT_WORKSPACE_NAME = "main"
# LINT.ThenChange(//build/bazel/toplevel.WORKSPACE.bazel)

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
_BAZEL_BUILTIN_REPOSITORIES = (
    "bazel_tools",
    "local_config_cc",
    "local_config_platform",
    # The two repositories below were added by Bazel 7.2.
    "host_platform",
    "internal_platforms_do_not_use",
    # Created and used internally by @rules_python
    "rules_python_internal",
)

# A list of file extensions for files that should be ignored from depfiles.
_IGNORED_FILE_SUFFIXES = (
    # .pyc files contain their own timestamp which does not necessarily match
    # their file timestamp, triggering the python interpreter to rewrite them
    # more or less randomly. Apart from that, their content always matches the
    # corresponding '.py' file which will always be listed as a real input,
    # so ignoring them is always safe.
    #
    # See https://stackoverflow.com/questions/23775760/how-does-the-python-interpreter-know-when-to-compile-and-update-a-pyc-file
    #
    ".pyc",
)

# A list of external repository names which do not require a hash content file
# I.e. their implementation should already record the right dependencies to
# their input files.
_BAZEL_NO_CONTENT_HASH_REPOSITORIES = (
    "fuchsia_build_config",
    "fuchsia_build_info",
    "gn_targets",
)

# Technical notes on input (source and build files) located in Bazel external
# repositories.
#
# This script uses Bazel queries to extract the list of implicit input files
# from Bazel. This list typically includes a very large number of files that
# are located in Bazel external repositories, for example, consider the
# following labels:
#
#    @bazel_skylib//lib:BUILD
#    @platforms//cpu:BUILD
#    @rules_cc/cc:find_cc_toolchain.bzl
#    @fuchsia_clang//:cc_toolchain_config.bzl
#    @fuchsia_sdk//:BUILD.bazel
#    @fuchsia_sdk//pkg/fdio:include/lib/fdio/vfs.h
#    @fuchsia_sdk//fidl/fuchsia.unknown:unknown.fidl
#    @internal_sdk//src/lib/fxl_fxl:command_line.cc
#
# These labels need to be converted to actual file paths in the Bazel
# output_base, which consists in converting the repository name '@foo' into a
# repository directory such as `$BAZEL_TOPDIR/output_base/external/foo`.
#
# Simply listing these paths in the depfile unfortunately triggers flaky Ninja
# no-op check failures on our infra builders during incremental builds.
# Because Bazel will sometimes decide to re-generate the content of a
# repository (by re-running its repository rule function) under certain
# conditions, which can be triggered semi-randomly between two consecutive
# Ninja build actions.
#
# Also many times a repository `@foo` is regenerated because one of its
# repository dependency (e.g. `@bar`) has to be updated, but the new content
# for `@foo` would be identical nonetheless.
#
# This regeneration is mostly unpredictable, but can be worked-around in the
# following way, implemented in the script below:
#
# - Most repository directories are symlinks that simply point outside of the
#   Bazel workspace. This happens for any repository declared with
#   `local_repository()` in the WORKSPACE.bazel file. These are never
#   regenerated. For example, for @bazel_skylib:
#
#     $TOPDIR/output_base/external/bazel_skylib
#        --symlink--> $TOPDIR/workspace/third_party/bazel_skylib
#          --symlink--> $FUCHSIA_DIR/third_party/bazel_skylib
#
#   In this case, replacing the repository directory with its final target
#   directly is safe and cannot trigger no-op failures, so:
#
#      @bazel_skylib//lib:BUILD
#          ---mapped--> ../../third_party/bazel_skylib/lib/BUILD
#
#      @platforms//cpu:BUILD
#          ---mapped--> ../../third_party/bazel_platforms/cpu/BUILD
#
#   There is one important exception though: the @bazel_tools repository which
#   is best ignored entirely (see comment above this one).
#
# - Many repositories, even if their content is generated by a repository rule,
#   contain files or directories that are symlinks pointing outside of the
#   Bazel workspace. For example:
#
#      @fuchsia_sdk//pkg/fdio:include/lib/fdio/vfs.h
#        --symlink--> $NINJA_OUTPUT_DIR/sdk/exported/core/pkg/fdio/include/lib/fdio/vfs.h
#          --symlink--> $FUCHSIA_DIR/sdk/lib/fdio/include/lib/fdio/vfs.h
#
#      @fuchsia_clang//:lib
#         --symlink--> $FUCHSIA_DIR/prebuilt/third_party/clang/{host_tag}/lib
#
#   It is sufficient to replace the label with the final link target, as in:
#
#      @fuchsia)sdk//pkg/fdio:include/lib/fdio/vfs.h
#        --mapped--> ../../sdk/lib/fdio/include/lib/fdio/vfs.h
#
#      @fuchsia_clang//:lib/x86_64-unknown/fuchsia/libunwind.so.1
#        --mapped--> ../../prebuilt/third_party/clang/{host_tag}/lib/x86_64-unknown-fuchsia/libunwind.so.1
#
#   There is still a minor issue here: what if re-generating the repository
#   would change the symlink's content (i.e. pointing to a different location,
#   replacing the symlink with a regular file, or removing the symlink?). Then
#   the depfile would have recorded a now stale, and thus incorrect, implicit
#   input dependency.
#
#   Most of the time, this will create un-necessary build actions on the next
#   Ninja invocation, and in rare cases, may even break the build. However,
#   this is no different from regular Ninja issues with depfiles (see
#   https://fxbug.dev/42164069), which cannot guarantee, by design, correct
#   incremental build.
#
#   So resolving the symlink seems an acceptable solution, as it would not be
#   worse than Ninja's current limitations.
#
# - Other files are generated by a repository rule, for example
#   @fuchsia_sdk//:BUILD.bazel, is generated by the fuchsia_sdk_repository()
#   rule implementation function, which reads the content of many input JSON
#   metadata files to determine what goes in that file.
#
#   Fortunately, this rule also uses a special file containing a content hash
#   for all these input metadata files, which is:
#
#     $BAZEL_TOPDIR/workspace/fuchsia_build_generated/fuchsia_sdk.hash
#
#   This file is generated by the `update-workspace.py` script which is invoked
#   from the GN //build/bazel:generate_main_workspace target, and which also
#   creates depfile entries to link the fuchsia_sdk.hash output file to
#   all input manifests.
#
#   Thus if any of the input manifest file changes, Ninja will force re-running
#   the //build/bazel:generate_main_workspace action, which will update the
#   content hash file, which will force a Bazel build command invoked from
#   this bazel_action.py script to regenerate all of @fuchsia_sdk//.
#
#   For the depfile entries generated by this script, it is thus enough to
#   replace any label to a generated (non-symlink) repository file, to the path
#   to the corresponding content hash file, if it exists, e.g.:
#
#      @fuchsia_sdk//:BUILD.bazel
#        --mapped-->  $BAZEL_TOPDIR/workspace/fuchsia_build_generated/fuchsia_sdk
#
#      @fuchsia_sdk//pkg/fdio:BUILD.bazel
#        --mapped-->  $BAZEL_TOPDIR/workspace/fuchsia_build_generated/fuchsia_sdk
#
#   There are several files under $BAZEL_TOPDIR/fuchsia_build_generated/,
#   each one matching a given repository name. So the algorithm used to map a
#   label like `@foo//:package/file` that does not point to a symlink is:
#
#      1) Lookup if a content hash file exists for repository @foo, e.g. look
#         for $BAZEL_TOPDIR/fuchsia_build_generated/foo.hash
#
#      2) If the content hash file exists, map the label directly to that file.
#
#            @foo//:package/file --mapped-->
#               $BAZEL_TOPDIR/fuchsia_build_generated/foo.hash
#
#      3) If the content hash file does not exist, print an error message
#         or ignore the file (controlled by the _ASSERT_ON_IGNORED_FILES
#         definition below). Asserting is preferred to detect these cases
#         as soon as possible, but can be disabled locally during development.
#
# - Some repository files are symlinks pointing to generated files belonging
#   to other external repositories, for example:
#
#      @fuchsia_sdk//tools/x64:cmc-meta.json
#         --symlink--> $OUTPUT_BASE/external/fuchsia_idk/tools/x64/cmc-meta-json
#
#   Which is a file generated by the @fuchsia_idk//tools/x64:cmc-meta.json
#   target.
#
#   In this case, the content hash file of the target repository should be used,
#   if it exists, or an error should be reported.
#
# - Finally, files whose name is a content-based hash are ignored as well,
#   because adding them to the depfile can only lead to stale depfile entries
#   that will trigger Ninja errors.
#
#   This is because if the corresponding content changes, the file's name
#   also changes, and the file's timestamp becomes irrelevant and can only
#   trigger un-necessary rebuilds. Moreover, a stale output dependency can
#   appear as a Ninja no-op error that looks like:
#
#   output obj/build/images/fuchsia/fuchsia/legacy/blobs/0a892655d873b0431095e9b51e1dad42b520af43db7ef76aa2a08074948a1565 of phony edge with no inputs doesn't exist
#
#   Detecting content-based file names looks for files which only include
#   hexadecimal chars, and of at least 16 chars long, or which include
#   a .build-id/xx/ prefix, where <xx> is a 2-char hexadecimal string.
#

# Set this to True to debug operations locally in this script.
# IMPORTANT: Setting this to True will result in Ninja timeouts in CQ
# due to the stdout/stderr logs being too large.
_DEBUG = False

# Set this to True to assert when non-symlink repository files are found.
# This is useful to find them when performing expensive builds on CQ
_ASSERT_ON_IGNORED_FILES = True

# The name of an environment variable that will be checked. If set
# to "1", this adds `--sandbox_debug` to each `bazel build` invocation
# in order to allow developers to see the content of sandboxes for
# failed commands.
# See https://blog.bazel.build/2016/03/18/sandbox-easier-debug.html
_ENV_DEBUG_SANDBOX = "FUCHSIA_DEBUG_BAZEL_SANDBOX"


def debug(msg: str) -> None:
    # Print debug message to stderr if _DEBUG is True.
    if _DEBUG:
        print("BAZEL_ACTION_DEBUG: " + msg, file=sys.stderr)


def get_input_starlark_file_path(filename: str) -> str:
    """Return the path of a input starlark file for Bazel queries.

    Args:
       filename: File name, searched in //build/bazel/starlark/
    Returns:
       file path to the corresponding file.
    """
    result = os.path.join(_STARLARK_DIR, filename)
    assert os.path.isfile(result), f"Missing starlark input file: {result}"
    return result


def make_writable(p: str) -> None:
    file_mode = os.stat(p).st_mode
    is_readonly = file_mode & stat.S_IWUSR == 0
    if is_readonly:
        os.chmod(p, file_mode | stat.S_IWUSR)


def copy_writable(src: str, dst: str) -> None:
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    shutil.copy2(src, dst)
    make_writable(dst)


def copy_directory_if_changed(
    src_dir: str, dst_dir: str, tracked_files: List[str]
) -> None:
    """Copy directory from |src_path| to |dst_path| if |tracked_files| have different mtimes.

    NOTE this function deliberately uses __mtime__, instead of content, of
    tracked_files to determine whether directories need a re-copy. This follows
    the convention many tools used in the build are using, where they use mtime
    of a file as a proxy for the freshness of a directory, because Ninja only
    understands timestamps.

    See http://b/365838961 for details.
    """
    assert os.path.isdir(
        src_dir
    ), "{} is not a dir, but copy dir is called.".format(src_path)

    def all_tracked_files_unchanged(
        src_dir: str, dst_dir: str, tracked_files: List[str]
    ) -> bool:
        """Use __mtime__ to determine whether any tracke file has changed.

        Returns true iff mtimes of tracked files are identical in src_dir and
        dst_dir.
        """
        for tracked_file in tracked_files:
            dst_tracked_file = os.path.join(dst_dir, tracked_file)
            if not os.path.exists(dst_tracked_file):
                return False
            src_tracked_file = os.path.join(src_dir, tracked_file)
            if os.path.getmtime(src_tracked_file) != os.path.getmtime(
                dst_tracked_file
            ):
                return False
        return True

    if all_tracked_files_unchanged(src_dir, dst_dir, tracked_files):
        return

    if os.path.lexists(dst_dir):
        shutil.rmtree(dst_dir)
    shutil.copytree(src_dir, dst_dir, copy_function=copy_writable)
    # Make directories writable so their contents can be removed and
    # repopulated in incremental builds.
    make_writable(dst_dir)
    for root, dirs, _ in os.walk(dst_dir):
        for d in dirs:
            make_writable(os.path.join(root, d))


def copy_file_if_changed(
    src_path: str, dst_path: str, bazel_output_base_dir: str
) -> None:
    """Copy file from |src_path| to |dst_path| if they are different."""
    assert os.path.isfile(
        src_path
    ), "{} is not a file, but copy file is called.".format(src_path)

    # NOTE: For some reason, filecmp.cmp() will return True if
    # dst_path does not exist, even if src_path is not empty!?
    if os.path.exists(dst_path) and filecmp.cmp(
        src_path, dst_path, shallow=False
    ):
        return

    # Use lexists to make sure broken symlinks are removed as well.
    if os.path.lexists(dst_path):
        os.remove(dst_path)

    def is_under_bazel_root(p):
        return os.path.realpath(p).startswith(
            os.path.abspath(bazel_output_base_dir)
        )

    # See https://fxbug.dev/42072059 for context. This logic is kept here
    # to avoid incremental failures when performing copies across
    # different revisions of the Fuchsia checkout (e.g. when bisecting
    # or simply in CQ).
    #
    # If the file is writable, and not a directory, try to hard-link it
    # directly. Otherwise, or if hard-linking fails due to a cross-device
    # link, do a simple copy.
    do_copy = True
    file_mode = os.stat(src_path).st_mode
    is_src_readonly = file_mode & stat.S_IWUSR == 0
    if not is_src_readonly:
        try:
            os.makedirs(os.path.dirname(dst_path), exist_ok=True)
            # Get realpath of src_path to avoid symlink chains, which
            # os.link does not handle properly even follow_symlinks=True.
            #
            # NOTE: it is important to link to the final real file because
            # intermediate links can be temporary. For example, the
            # gn_targets repository is repopulated in every bazel_action, so
            # any links pointing to symlinks in gn_targets can be
            # invalidated during the build.
            os.link(os.path.realpath(src_path), dst_path)
            # Update timestamp to avoid Ninja no-op failures that can
            # happen because Bazel does not maintain consistent timestamps
            # in the execroot when sandboxing or remote builds are enabled.
            if is_under_bazel_root(src_path):
                os.utime(dst_path)
            do_copy = False
        except OSError as e:
            if e.errno != errno.EXDEV:
                raise

    if do_copy:
        copy_writable(src_path, dst_path)


def force_symlink(target_path: str, link_path: str) -> None:
    if os.path.islink(link_path) or os.path.exists(link_path):
        os.remove(link_path)

    link_parent = os.path.dirname(link_path)
    os.makedirs(link_parent, exist_ok=True)
    rel_target_path = os.path.relpath(target_path, link_parent)
    os.symlink(rel_target_path, link_path)

    real_dst_path = os.path.realpath(link_path)
    real_src_path = os.path.realpath(target_path)
    assert (
        real_dst_path == real_src_path
    ), f"Symlink creation failed {link_path} points to {real_dst_path}, not {real_src_path}"


def depfile_quote(path: str) -> str:
    """Quote a path properly for depfiles, if necessary.

    shlex.quote() does not work because paths with spaces
    are simply encased in single-quotes, while the Ninja
    depfile parser only supports escaping single chars
    (e.g. ' ' -> '\ ').

    Args:
       path: input file path.
    Returns:
       The input file path with proper quoting to be included
       directly in a depfile.
    """
    return path.replace("\\", "\\\\").replace(" ", "\\ ")


_HEXADECIMAL_SET = set("0123456789ABCDEFabcdef")


def is_hexadecimal_string(s: str) -> bool:
    """Return True if input string only contains hexadecimal characters."""
    return all([c in _HEXADECIMAL_SET for c in s])


_BUILD_ID_PREFIX = ".build-id/"


def is_likely_build_id_path(path: str) -> bool:
    """Return True if path is a .build-id/xx/yyyy* file name."""
    # Look for .build-id/XX/ where XX is an hexadecimal string.
    pos = path.find(_BUILD_ID_PREFIX)
    if pos < 0:
        return False

    path = path[pos + len(_BUILD_ID_PREFIX) :]
    if len(path) < 3 or path[2] != "/":
        return False

    return is_hexadecimal_string(path[0:2])


assert is_likely_build_id_path("/src/.build-id/ae/23094.so")
assert not is_likely_build_id_path("/src/.build-id/log.txt")


def copy_build_id_dir(build_id_dir: str, bazel_output_base_dir: str) -> None:
    """Copy debug symbols from a source .build-id directory, to the top-level one."""
    for path in os.listdir(build_id_dir):
        bid_path = os.path.join(build_id_dir, path)
        if len(path) == 2 and os.path.isdir(bid_path):
            for obj in os.listdir(bid_path):
                src_path = os.path.join(bid_path, obj)
                dst_path = os.path.join(_BUILD_ID_PREFIX, path, obj)
                copy_file_if_changed(src_path, dst_path, bazel_output_base_dir)


def is_likely_content_hash_path(path: str) -> bool:
    """Return True if file path is likely based on a content hash.

    Args:
        path: File path
    Returns:
        True if the path is a likely content-based file path, False otherwise.
    """
    # Look for .build-id/XX/ where XX is an hexadecimal string.
    if is_likely_build_id_path(path):
        return True

    # Look for a long hexadecimal sequence filename.
    filename = os.path.basename(path)
    return len(filename) >= 16 and is_hexadecimal_string(filename)


assert is_likely_content_hash_path("/src/.build-id/ae/23094.so")
assert not is_likely_content_hash_path("/src/.build-id/log.txt")


def find_bazel_output_base(workspace_dir: str) -> str:
    """Return the path of the Bazel output base."""
    return os.path.normpath(
        os.path.join(os.path.dirname(workspace_dir), "output_base")
    )


def find_bazel_execroot(workspace_dir: str) -> str:
    """Return the path of the Bazel execroot."""
    return os.path.join(
        find_bazel_output_base(workspace_dir),
        "execroot",
        _BAZEL_ROOT_WORKSPACE_NAME,
    )


def remove_gn_toolchain_suffix(gn_label: str) -> str:
    """Remove the toolchain suffix of a GN label."""
    return gn_label.partition("(")[0]


class BazelLabelMapper(object):
    """Provides a way to map Bazel labels to file paths.

    Usage is:
      1) Create instance, passing the path to the Bazel workspace.
      2) Call source_label_to_path(<label>) where the label comes from
         a query.
    """

    def __init__(self, bazel_workspace: str, output_dir: str):
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
        self._output_dir = os.path.abspath(output_dir)
        output_base = os.path.normpath(
            os.path.join(bazel_workspace, "..", "output_base")
        )
        self._output_base = output_base

        assert os.path.isdir(output_base), f"Missing directory: {output_base}"
        self._external_dir_prefix = (
            os.path.realpath(os.path.join(output_base, "external")) + "/"
        )

        # Some repositories have generated files that are associated with
        # a content hash file generated by update-workspace.py. This map is
        # used to return the path to such file if it exists, or an empty
        # string otherwise.
        self._repository_hash_map: Dict[str, str] = {}

    def _get_repository_content_hash(self, repository_name: str) -> str:
        """Check whether a repository name has an associated content hash file.

        Args:
            repository_name: Bazel repository name, must start with an @,
               e.g. '@foo' or '@@foo.1.0'

        Returns:
            If the corresponding repository has a content hash file, return
            its path. Otherwise, return an empty string.
        """
        hash_file = self._repository_hash_map.get(repository_name, None)
        if hash_file is None:
            # Canonical names like @@foo.<version> need to be converted to just `foo` here.
            file_prefix = repository_name[1:]
            if file_prefix.startswith("@"):
                name, dot, version = file_prefix[1:].partition(".")
                if dot == ".":
                    file_prefix = name
                else:
                    # No version, get rid of initial @@
                    file_prefix = file_prefix[1:]

            # First look into $BUILD_DIR/regenerator_outputs/bazel_content_hashes/
            # then into $WORKSPACE/fuchsia_build_generated/
            hash_file = os.path.join(
                self._output_dir,
                "regenerator_outputs",
                "bazel_content_hashes",
                file_prefix + ".hash",
            )
            if not os.path.exists(hash_file):
                hash_file = os.path.join(
                    self._root_workspace,
                    "fuchsia_build_generated",
                    file_prefix + ".hash",
                )
                if not os.path.exists(hash_file):
                    hash_file = ""

            self._repository_hash_map[repository_name] = hash_file

        return hash_file

    def source_label_to_path(
        self, label: str, relative_to: str | None = None
    ) -> str:
        """Convert a Bazel label to a source file into the corresponding file path.

        Args:
          label: A fully formed Bazel label, as return by a query. If BzlMod is
              enabled, this expects canonical repository names to be present
              (e.g. '@foo.12//src/lib:foo.cc' and no '@foo//src/lib:foo.cc').
          relative_to: Optional directory path string.
        Returns:
          If relative_to is None, the absolute path to the corresponding source
          file, otherwise, the same path relative to `relative_to`.

          This returns an empty string if the file should be ignored, i.e.
          not added to the depfile.
        """
        #
        # NOTE: Only the following input label formats are supported
        #
        #    //<package>:<target>
        #    @//<package>:<target>
        #    @<name>//<package>:<target>
        #    @@<name>//<package>:<target>
        #    @@<name>.<version>//<package>:<target>
        #
        repository, sep, package_label = label.partition("//")
        assert sep == "//", f"Missing // in source label: {label}"
        if repository == "" or repository == "@":
            # @// references the root project workspace, it should normally
            # not appear in queries, but handle it here just in case.
            #
            # // references a path relative to the current workspace, but the
            # queries are always performed from the root project workspace, so
            # is equivalent to @// for this function.
            repository_dir = self._root_workspace
            from_external_repository = False
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
            assert repository.startswith(
                "@"
            ), f"Invalid repository name in source label {label}"

            # @@ is used with canonical repo names, so remove both @@ and @.
            repository_name = repository.removeprefix("@@").removeprefix("@")
            repository_dir = self._external_dir_prefix + repository_name
            from_external_repository = True

        package, colon, target = package_label.partition(":")
        assert colon == ":", f"Missing colon in source label {label}"
        path = os.path.join(repository_dir, package, target)

        # Check whether this path is a symlink to something else.
        # Use os.path.realpath() which always return an absolute path
        # after resolving all symlinks to their final destination, then
        # compare it with os.path.abspath(path):
        real_path = os.path.realpath(path)
        if real_path != os.path.abspath(path):
            # This is symlink, so first resolve it to its destination.
            path = real_path

            # If the symlink points to another external repository, try
            # to find a content hash file for it, or return an empty path.
            if path.startswith(self._external_dir_prefix):
                # This path points to generated files in another Bazel external
                # repository. Check if the latter  has a content hash file, or
                # return an empty path.
                repo_path = path[len(self._external_dir_prefix) :]
                repo_name, sep, _ = repo_path.partition("/")
                assert (
                    sep == "/"
                ), f"Unexpected external repository path: external/{repo_path} (from {label})"
                path = self._get_repository_content_hash("@" + repo_name)

        elif from_external_repository:
            # This is generated file inside an external repository. Find a content
            # hash file for it, or return an empty path.
            path = self._get_repository_content_hash(repository)

        if path:
            assert os.path.isabs(
                path
            ), f"Unexpected non-absolute path: {path} (from {label})"

            # Check that the translated path does not point into the output_base
            # as timestamps in this directory are not guaranteed to be consistent.
            assert not path.startswith(
                self._output_base
            ), f"Path should not be in Bazel output_base: {path} (from {label})"

            if relative_to:
                path = os.path.relpath(path, relative_to)

        return path


def verify_unknown_gn_targets(
    build_files_error: List[str],
    gn_action_target: str,
    bazel_targets: List[str],
) -> int:
    """Check for unknown @gn_targets// dependencies.

    Args:
        build_files_error: list of error lines from bazel build or query.
        gn_action_target: Label of the GN bazel_action() target for this invocation.
        bazel_targets: List of Bazel targets invoked by the GN bazel_action() target.

    Returns:
        On success, simply return 0. On failure, print a human friendly
        error message explaining the situation to stderr, then return 1.
    """
    missing_ninja_outputs = set()
    missing_ninja_packages = set()
    build_dirs: Set[str] = set()
    for error_line in build_files_error:
        if not ("ERROR: " in error_line and "@gn_targets//" in error_line):
            continue

        pos = error_line.find("@@gn_targets//")
        if pos < 0:
            # Should not happen, do not assert and let the caller print the full error
            # after this.
            print(f"UNSUPPORTED ERROR LINE: {error_line}", file=sys.stderr)
            return 0

        ending_pos = error_line.find("'", pos)
        if ending_pos < 0:
            print(f"UNSUPPORTED ERROR LINE: {error_line}", file=sys.stderr)
            return 0

        label = error_line[pos + 1 : ending_pos]  # skip first @.
        if error_line[:pos].endswith(": no such package '"):
            # The line looks like the following when the BUILD.bazel references a label
            # that does not belong to a @gn_targets package.
            #
            # ERROR: <abspath>/BUILD.bazel:<line>:<column>: no such package '@@gn_targets//<dir>': ...
            #
            # This happens when the GN bazel_action() targets fails to dependon the corresponding
            # bazel_input_file() or bazel_input_directory() target, and that none of its other
            # dependencies expose other targets from the same package / directory. The error message
            # does not give any information about the target name being evaluated by the query though.
            missing_ninja_packages.add(label)

        elif error_line[:pos].endswith(": no such target '"):
            # The line looks like this when the BUILD.bazel files references the wrong
            # label from a package exposed in @gn_targets//:
            # ERROR: <abspath>/BUILD.bazel:<line>:<column>: no such target '@@gn_targets//<dir>:<name>' ...
            missing_ninja_outputs.add(label)

    if not missing_ninja_outputs and not missing_ninja_packages:
        return 0

    missing_outputs = sorted(missing_ninja_outputs)
    missing_packages = sorted(missing_ninja_packages)

    _ERROR = """
BAZEL_ACTION_ERROR: Unknown @gn_targets targets.

The following GN target:               {gn_target}
Builds the following Bazel target(s):  {bazel_targets}
""".format(
        gn_target=gn_action_target,
        bazel_targets=" ".join(bazel_targets),
    )

    if not missing_packages:
        _ERROR += """Which references the following unknown @gn_targets labels:

  {missing_bazel_labels}

To fix this, ensure that bazel_input_file() or bazel_input_directory()
targets are defined in the GN graph for:

  {missing_gn_labels}

Then ensure that the GN target depends on them transitively.
""".format(
            missing_bazel_labels="\n  ".join(missing_outputs),
            missing_gn_labels="\n  ".join(
                f"//{o.removeprefix('@gn_targets//')}" for o in missing_outputs
            ),
        )

    else:
        missing_labels = missing_outputs + missing_packages
        missing_build_files = set()
        for label in missing_labels:
            label = label.removeprefix("@gn_targets//")
            gn_dir, sep, gn_name = label.partition(":")
            if sep != ":":
                gn_dir = label
            missing_build_files.add(f"//{gn_dir}/BUILD.gn")

        _ERROR += """Which references the following unknown @gn_targets labels or packages:

  {missing_bazel_labels}

To fix this, ensure that bazel_input_file() or bazel_input_directory()
targets are defined in the following build files:

  {missing_build_files}

Then ensure that the GN target depends on them transitively.
""".format(
            missing_bazel_labels="\n  ".join(
                missing_outputs + missing_packages
            ),
            missing_build_files="\n  ".join(sorted(missing_build_files)),
        )

    print(_ERROR, file=sys.stderr)
    return 1


def repository_name(label: str) -> str:
    """Returns repository name of the input label.

    Supports both canonical repository names (starts with @@) and apparent
    repository names (starts with @).
    """
    repository, sep, _ = label.partition("//")
    assert sep == "//", f"Missing // in label: {label}"
    return repository.removeprefix("@@").removeprefix("@")


def is_ignored_input_label(label: str) -> bool:
    """Return True if the label of a build or source file should be ignored."""
    is_builtin = repository_name(label) in _BAZEL_BUILTIN_REPOSITORIES
    is_ignored = label.endswith(_IGNORED_FILE_SUFFIXES)
    return is_builtin or is_ignored


def label_requires_content_hash(label: str) -> bool:
    """Return True if the label or source file belongs to a repository
    that requires a content hash file."""
    return not (repository_name(label) in _BAZEL_NO_CONTENT_HASH_REPOSITORIES)


def list_to_pairs(l: Iterable[Any]) -> Iterable[Tuple[Any, Any]]:
    is_first = True
    for val in l:
        if is_first:
            last_val = val
            is_first = False
        else:
            yield (last_val, val)
            is_first = True


@dataclasses.dataclass
class FileOutputs:
    bazel_path: str
    ninja_path: str


class FileOutputsAction(argparse.Action):
    """ArgumentParser action class to convert --file-outputs arguments into FileOutputs instances."""

    def __init__(  # type: ignore
        self, option_strings, dest, nargs=None, default=None, **kwargs
    ):
        if nargs is not None:
            raise ValueError("nargs not allowed")
        if default is not None:
            raise ValueError("default not allowed")
        super().__init__(option_strings, dest, nargs="+", default=[], **kwargs)

    def __call__(self, parser, namespace, values, option_string):  # type: ignore
        if len(values) < 2:
            raise ValueError(
                f"expected at least 2 arguments for {option_string}"
            )
        if len(values) & 1 != 0:
            raise ValueError(
                f"expected an even number of arguments for {option_string}"
            )
        dest_list = getattr(namespace, self.dest, [])
        dest_list.extend(
            [
                FileOutputs(bazel_path, ninja_path)
                for bazel_path, ninja_path in list_to_pairs(values)
            ]
        )
        setattr(namespace, self.dest, dest_list)


@dataclasses.dataclass
class DirectoryOutputs:
    bazel_path: str
    ninja_path: str
    tracked_files: List[str] = dataclasses.field(default_factory=list)
    copy_debug_symbols: bool = False


class DirectoryOutputsAction(argparse.Action):
    """ArgumentParser action class to convert --directory-outputs arguments into DirectoryOutputs instances."""

    def __init__(  # type: ignore
        self, option_strings, dest, nargs=None, default=None, **kwargs
    ):
        if nargs is not None:
            raise ValueError("nargs not allowed")
        if default is not None:
            raise ValueError("default not allowed")
        super().__init__(option_strings, dest, nargs="+", default=[], **kwargs)

    def __call__(self, parser, namespace, values, option_string):  # type: ignore
        if len(values) < 3:
            raise ValueError(
                f"expected at least 3 arguments for {option_string}"
            )
        dest_list = getattr(namespace, self.dest, [])
        bazel_path = values[0]
        ninja_path = values[1]
        copy_debug_symbols = bool(values[2] == "true")
        tracked_files = values[3:]
        dest_list.append(
            DirectoryOutputs(
                bazel_path, ninja_path, tracked_files, copy_debug_symbols
            )
        )
        setattr(namespace, self.dest, dest_list)


@dataclasses.dataclass
class FinalSymlinkOutputs:
    bazel_path: str
    ninja_path: str


class FinalSymlinkOutputsAction(argparse.Action):
    """ArgumentParser action class to convert --final-symlink-outputs arguments into a FinalSymlinkOutputs instances."""

    def __init__(  # type: ignore
        self, option_strings, dest, nargs=None, default=None, **kwargs
    ):
        if nargs is not None:
            raise ValueError("nargs not allowed")
        if default is not None:
            raise ValueError("default not allowed")
        super().__init__(option_strings, dest, nargs="+", default=[], **kwargs)

    def __call__(self, parser, namespace, values, option_string):  # type: ignore
        if len(values) != 2:
            raise ValueError(f"expected 2 arguments for {option_string}")
        dest_list = getattr(namespace, self.dest, [])
        bazel_path = values[0]
        ninja_path = values[1]
        dest_list.append(FinalSymlinkOutputs(bazel_path, ninja_path))
        setattr(namespace, self.dest, dest_list)


@dataclasses.dataclass
class PackageOutputs:
    package_label: str
    archive_path: Optional[str] = None
    manifest_path: Optional[str] = None
    copy_debug_symbols: bool = False


class PackageOutputsAction(argparse.Action):
    """ArgumentParser action class to convert --package-outputs arguments into PackageOutputs instances."""

    def __init__(  # type: ignore
        self, option_strings, dest, nargs=None, default=None, **kwargs
    ):
        if nargs is not None:
            raise ValueError("nargs not allowed")
        if default is not None:
            raise ValueError("default not allowed")
        super().__init__(option_strings, dest, nargs=4, default=[], **kwargs)

    def __call__(self, parser, namespace, values, option_string):  # type: ignore
        assert len(values) == 4
        if not values[0]:
            raise ValueError("expected non-empty Bazel package label.")
        dest_list = getattr(namespace, self.dest, [])
        package_label = values[0]
        archive_path = values[1] if values[1] != "NONE" else None
        manifest_path = values[2] if values[2] != "NONE" else None
        copy_debug_symbols = bool(values[3] == "true")
        dest_list.append(
            PackageOutputs(
                package_label, archive_path, manifest_path, copy_debug_symbols
            )
        )
        setattr(namespace, self.dest, dest_list)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bazel-launcher", required=True, help="Path to Bazel launcher script."
    )
    parser.add_argument(
        "--workspace-dir", required=True, help="Bazel workspace path"
    )
    parser.add_argument(
        "--command",
        required=True,
        help="Bazel command, e.g. `build`, `run`, `test`",
    )
    parser.add_argument(
        "--gn-target-label",
        required=True,
        help="Label of GN target invoking this script.",
    )
    parser.add_argument(
        "--ninja-inputs-manifest",
        help="Path to the manifest file describing bazel_input_xxx() dependencies.",
    )
    parser.add_argument(
        "--gn-targets-repository-manifest",
        help="Path to the manifest file describing the content of the @gn_targets repository.",
    )
    parser.add_argument(
        "--gn-targets-all-licenses-spdx",
        help="Path to the SPDX file describing licenses for all files in @gn_targets repository.",
    )
    parser.add_argument(
        "--bazel-targets",
        default=[],
        nargs="*",
        help="List of bazel target patterns.",
    )
    parser.add_argument(
        "--stamp-files",
        default=[],
        nargs="*",
        help="List of stamp files to touch.",
    )
    parser.add_argument(
        "--file-outputs",
        action=FileOutputsAction,
        help="A list of (bazel_path, ninja_path) file paths.",
    )
    parser.add_argument(
        "--directory-outputs",
        action=DirectoryOutputsAction,
        help="4 or more arguments to specify a single Bazel output directory. Begins with (bazel_path, ninja_path, copy_debug_symbols) values, followed by one or more tracked relative file",
    )
    parser.add_argument(
        "--package-outputs",
        action=PackageOutputsAction,
        help="A tuple of four values describing Fuchsia package related outputs. Fields are Bazel package target label, archive output path or 'NONE', manifest output path or 'NONE', and copy debug symbols flag as either 'true' or 'false' string",
    )
    parser.add_argument(
        "--final-symlink-outputs",
        action=FinalSymlinkOutputsAction,
        help="A (bazel_path, ninja_path) pair to specify a single final symlink to a Bazel artifact.",
    )

    parser.add_argument(
        "--bazel-build-events-log-json",
        help="Path to JSON formatted event log for build actions.",
    )

    parser.add_argument("--depfile", help="Ninja depfile output path.")
    parser.add_argument(
        "--allow-directory-in-outputs",
        action="store_true",
        default=False,
        help="Allow directory outputs in `--bazel-outputs`, NOTE timestamps on directories do NOT accurately reflect content freshness, which can lead to incremental build incorrectness.",
    )
    parser.add_argument(
        "--path-mapping",
        help="If specified, write a mapping of Ninja outputs to realpaths Bazel outputs",
    )
    parser.add_argument("extra_bazel_args", nargs=argparse.REMAINDER)

    args = parser.parse_args()

    if not args.bazel_targets:
        return parser.error("A least one --bazel-targets value is needed!")

    _build_fuchsia_package = args.command == "build" and args.package_outputs

    if args.extra_bazel_args and args.extra_bazel_args[0] != "--":
        return parser.error(
            "Extra bazel args should be separated from script args using --"
        )
    args.extra_bazel_args = args.extra_bazel_args[1:]

    if not os.path.exists(args.workspace_dir):
        return parser.error(
            "Workspace directory does not exist: %s" % args.workspace_dir
        )

    if not os.path.exists(args.bazel_launcher):
        return parser.error(
            "Bazel launcher does not exist: %s" % args.bazel_launcher
        )

    current_dir = os.getcwd()
    bazel_output_base_dir = find_bazel_output_base(args.workspace_dir)

    def relative_workspace_dir(path: str) -> str:
        """Convert path relative to the workspace root to the path relative to current directory."""
        return os.path.relpath(
            os.path.abspath(
                os.path.realpath(os.path.join(args.workspace_dir, path))
            )
        )

    # LINT.IfChange
    if args.gn_targets_repository_manifest:
        force_symlink(
            target_path=args.gn_targets_repository_manifest,
            link_path=os.path.join(
                args.workspace_dir,
                "fuchsia_build_generated/gn_targets_repository_manifest.json",
            ),
        )

    if args.gn_targets_all_licenses_spdx:
        force_symlink(
            target_path=args.gn_targets_all_licenses_spdx,
            link_path=os.path.join(
                args.workspace_dir,
                "fuchsia_build_generated/gn_targets_all_licenses_spdx.json",
            ),
        )
    # LINT.ThenChange(//build/bazel/toplevel.WORKSPACE.bazel)

    jobs = None
    if "--config=remote" in args.extra_bazel_args:
        cpus = os.cpu_count()
        if cpus:
            jobs = 10 * cpus

    if jobs is None:
        # If an explicit job count was passed to `fx build`, tell Bazel to respect it.
        # See https://fxbug.dev/351623259
        job_count = os.environ.get("FUCHSIA_BAZEL_JOB_COUNT")
        if job_count:
            jobs = int(job_count)

    def run_bazel_query(
        query_type: str, query_args: List[str], ignore_errors: bool
    ) -> "subprocess.CompletedProcess[str]":
        """Run a Bazel query, and return stdout, stderr pair.

        Args:
           query_type: One of 'query', 'cquery' or 'aquery'.
           query_args: Additional query arguments.
           ignore_errors: If true, errors are ignored.

        Returns:
            On success, return an (stdout_lines, stderr_lines) pair, where
            stdout_lines is a list of output lines, and stderr_lines is a list
            of error lines, if any.

            On failure, if ignore_errors is False, then return (None, None),
            otherwise, return (stdout_lines, stderr_lines).
        """
        query_cmd = [
            args.bazel_launcher,
            query_type,
        ] + query_args

        if ignore_errors:
            query_cmd += ["--keep_going"]

        query_cmd_str = " ".join(shlex.quote(c) for c in query_cmd)
        debug("QUERY_CMD: " + query_cmd_str)
        ret = subprocess.run(query_cmd, capture_output=True, text=True)
        if ret.returncode != 0:
            if not ignore_errors:
                print(
                    "BAZEL_ACTION_ERROR: Error when calling Bazel query: %s\n%s\n%s\n"
                    % (query_cmd_str, ret.stderr, ret.stdout),
                    file=sys.stderr,
                )
                return ret

            if _DEBUG and False:
                print(
                    "BAZEL_ACTION_WARNING: Error when calling Bazel query: %s\nSTDOUT\n%s\nSTDERR\n%s\n"
                    % (query_cmd_str, ret.stderr, ret.stdout),
                    file=sys.stderr,
                )

        return ret

    def get_bazel_query_output(
        query_type: str, query_args: List[str]
    ) -> Optional[List[str]]:
        """Run a bazel query and return its output as a series of lines.

        Args:
            query_type: One of 'query', 'cquery' or 'aquery'
            query_args: Extra query arguments.

        Returns:
            On success, a list of output lines. On failure return None.
        """
        ret = run_bazel_query(query_type, query_args, False)
        if ret.returncode != 0:
            return None
        else:
            return ret.stdout.splitlines()

    configured_args = [shlex.quote(arg) for arg in args.extra_bazel_args]

    def run_starlark_cquery(
        query_target: str, starlark_filename: str
    ) -> List[str]:
        result = get_bazel_query_output(
            "cquery",
            [
                "--config=quiet",
                "--output=starlark",
                "--starlark:file",
                get_input_starlark_file_path(starlark_filename),
                query_target,
            ]
            + configured_args,
        )
        assert result is not None
        return result

    # All bazel targets as a set() expression for Bazel queries below.
    # See https://bazel.build/query/language#set
    query_targets = "set(%s)" % " ".join(args.bazel_targets)

    # Generate the genquery target for listing all build files that we need to
    # include in the depfile for this target, i.e. any changes in these build
    # files should trigger a rebuild of this target.
    genquery_tmpl = os.path.join(_TEMPLATE_DIR, "template.genrule.bazel")
    output_build_file = os.path.join(
        args.workspace_dir, "buildfiles_genquery", "BUILD.bazel"
    )
    os.makedirs(os.path.dirname(output_build_file), exist_ok=True)
    with open(genquery_tmpl, "r") as tmpl:
        with open(output_build_file, "w") as out:
            out.write(
                tmpl.read().format(
                    query_expression=f"buildfiles(deps({query_targets}))",
                    query_scopes=",".join(
                        (f'"{s}"' for s in args.bazel_targets)
                    ),
                    query_opts='"--output=label"',
                )
            )

    cmd = [args.bazel_launcher, args.command]

    if args.bazel_build_events_log_json:
        # Create parent directory to avoid Bazel complaining it cannot
        # write the events log file.
        os.makedirs(
            os.path.dirname(args.bazel_build_events_log_json), exist_ok=True
        )
        cmd += [
            "--build_event_json_file="
            + os.path.abspath(args.bazel_build_events_log_json),
        ]

    cmd += configured_args
    cmd += ["//buildfiles_genquery:genquery"]
    cmd += args.bazel_targets
    # Always use --verbose_failures to get relevant information when
    # Bazel commands fail. This is necessary to make the log output of
    # CQ/CI bots usable.
    cmd += ["--verbose_failures"]

    # Add --sandbox_debug if FUCHSIA_DEBUG_BAZEL_SANDBOX=1 is
    # in the environment.
    if os.environ.get(_ENV_DEBUG_SANDBOX, "0") == "1":
        cmd.append("--sandbox_debug")

    if any(
        entry.copy_debug_symbols
        for entry in args.package_outputs + args.directory_outputs
    ):
        # Ensure the build_id directories are produced.
        cmd += ["--output_groups=+build_id_dirs"]

    if jobs:
        cmd += [f"--jobs={jobs}"]

    if _DEBUG:
        debug("BUILD_CMD: " + " ".join(shlex.quote(c) for c in cmd))

    # NOTE: It is important to NOT capture output from this subprocess, to make
    # sure console output from Bazel are correctly printed out by Ninja.
    ret = subprocess.run(cmd)
    if ret.returncode != 0:
        # Detect the error message corresponding to a Bazel target
        # referencing a @gn_targets//<dir>:<name> label
        # that does not exist. This happens when the GN bazel_action()
        # fails to depend on the proper bazel_input_file() or
        # bazel_input_directory() dependency.
        #
        # NOTE: Path to command.log should be stable, because we explicitly set
        # output_base. See https://bazel.build/run/scripts#command-log.
        with open(os.path.join(bazel_output_base_dir, "command.log"), "r") as f:
            if verify_unknown_gn_targets(
                f.read().splitlines(),
                args.gn_target_label,
                args.bazel_targets,
            ):
                return 1

        # This is a different error, just print it as is.
        print(
            "\nERROR when calling Bazel. To reproduce, run this in the Ninja output directory:\n\n  %s\n"
            % " ".join(shlex.quote(c) for c in cmd),
            file=sys.stderr,
        )
        return 1

    if args.command == "build":
        build_files_query_output = os.path.join(
            args.workspace_dir, "bazel-bin", "buildfiles_genquery", "genquery"
        )
        with open(build_files_query_output, "r") as f:
            build_files = f.read().splitlines()

        bazel_source_files = get_bazel_query_output(
            "cquery",
            [
                "--config=quiet",
                "--output",
                "label",
                f'kind("source file", deps({query_targets}))',
            ]
            + configured_args,
        )

        if bazel_source_files is None:
            return 1

        if _DEBUG:
            debug("SOURCE FILES:\n%s\n" % "\n".join(bazel_source_files))

        # Remove the ' (null)' suffix of each result line.
        source_files = [l.partition(" (null)")[0] for l in bazel_source_files]

    file_copies = []
    unwanted_dirs = []

    for file_output in args.file_outputs:
        src_path = os.path.join(args.workspace_dir, file_output.bazel_path)
        if os.path.isdir(src_path):
            unwanted_dirs.append(src_path)
            continue
        dst_path = file_output.ninja_path
        file_copies.append((src_path, dst_path))

    if unwanted_dirs:
        print(
            "\nDirectories are not allowed in --file-outputs Bazel paths, got directories:\n\n%s\n"
            % "\n".join(unwanted_dirs)
        )
        return 1

    final_symlinks = []
    for final_symlink_output in args.final_symlink_outputs:
        src_path = os.path.join(
            os.path.join(args.workspace_dir, final_symlink_output.bazel_path)
        )
        target_path = os.path.realpath(src_path)
        link_path = final_symlink_output.ninja_path
        final_symlinks.append((target_path, link_path))

    if _build_fuchsia_package:
        bazel_execroot = find_bazel_execroot(args.workspace_dir)

        for entry in args.package_outputs:
            if not (
                entry.archive_path
                or entry.manifest_path
                or entry.copy_debug_symbols
            ):
                continue

            # Run a cquery to extract the FuchsiaPackageInfo and
            # FuchsiaDebugSymbolInfo provider values.
            query_result = run_starlark_cquery(
                entry.package_label,
                "package_archive_manifest_and_debug_symbol_dirs.cquery",
            )
            assert (
                len(query_result) > 2
            ), f"Unexpected FuchsiaPackageInfo cquery result: {query_result}"

            # Get all paths, which are relative to the Bazel execroot.
            bazel_archive_path, bazel_manifest_path = query_result[:2]
            bazel_debug_symbol_dirs = query_result[2:]

            if entry.archive_path:
                file_copies.append(
                    (
                        os.path.join(bazel_execroot, bazel_archive_path),
                        entry.archive_path,
                    )
                )

            if entry.manifest_path:
                file_copies.append(
                    (
                        os.path.join(bazel_execroot, bazel_manifest_path),
                        entry.manifest_path,
                    )
                )

            if entry.copy_debug_symbols:
                for debug_symbol_dir in bazel_debug_symbol_dirs:
                    copy_build_id_dir(
                        os.path.join(bazel_execroot, debug_symbol_dir),
                        bazel_output_base_dir,
                    )

    for src_path, dst_path in file_copies:
        copy_file_if_changed(src_path, dst_path, bazel_output_base_dir)

    for target_path, link_path in final_symlinks:
        force_symlink(target_path, link_path)

    dir_copies = []
    unwanted_files = []
    invalid_tracked_files = []

    for dir_output in args.directory_outputs:
        src_path = os.path.join(args.workspace_dir, dir_output.bazel_path)
        if not os.path.isdir(src_path):
            unwanted_files.append(src_path)
            continue
        for tracked_file in dir_output.tracked_files:
            tracked_file = os.path.join(src_path, tracked_file)
            if not os.path.isfile(tracked_file):
                invalid_tracked_files.append(tracked_file)
        dst_path = dir_output.ninja_path
        dir_copies.append((src_path, dst_path, dir_output.tracked_files))

        if dir_output.copy_debug_symbols:
            bazel_execroot = find_bazel_execroot(args.workspace_dir)
            debug_symbol_dirs = run_starlark_cquery(
                args.bazel_targets[0],
                "FuchsiaDebugSymbolInfo_debug_symbol_dirs.cquery",
            )
            for debug_symbol_dir in debug_symbol_dirs:
                copy_build_id_dir(
                    os.path.join(bazel_execroot, debug_symbol_dir),
                    bazel_output_base_dir,
                )

    if unwanted_files:
        print(
            "\nNon-directories are not allowed in --directory-outputs Bazel path, got:\n\n%s\n"
            % "\n".join(unwanted_files)
        )
        return 1

    if invalid_tracked_files:
        print(
            "\nMissing or non-directory tracked files from --directory-outputs Bazel path:\n\n%s\n"
            % "\n".join(invalid_tracked_files)
        )
        return 1

    for src_path, dst_path, tracked_files in dir_copies:
        copy_directory_if_changed(src_path, dst_path, tracked_files)

    # Drop tracked_files from dir_copies so it can be concatenated with
    # file_copies later.
    dir_copies = [(src, dst) for src, dst, _ in dir_copies]

    if args.path_mapping:
        # When determining source path of the copied output, follow links to get
        # out of bazel-bin, because the content of bazel-bin is not guaranteed
        # to be stable after subsequent `bazel` commands.
        with open(args.path_mapping, "w") as f:
            f.write(
                "\n".join(
                    dst_path
                    + ":"
                    + os.path.relpath(os.path.realpath(src_path), current_dir)
                    for src_path, dst_path in file_copies + dir_copies
                )
            )

    if args.depfile:
        # Perform a cquery to get all source inputs for the targets, this
        # returns a list of Bazel labels followed by "(null)" because these
        # are never configured. E.g.:
        #
        #  //build/bazel/examples/hello_world:hello_world (null)
        #
        mapper = BazelLabelMapper(args.workspace_dir, current_dir)

        all_inputs = [
            label
            for label in build_files + source_files
            if not is_ignored_input_label(label)
        ]

        # Convert output labels to paths relative to the current directory.
        if _DEBUG:
            debug("ALL INPUTS:\n%s\n" % "\n".join(all_inputs))

        ignored_labels = []
        all_sources = set()
        for label in all_inputs:
            path = mapper.source_label_to_path(label, relative_to=current_dir)
            if path:
                if is_likely_content_hash_path(path):
                    debug(f"{path} ::: IGNORED CONTENT HASH NAME")
                else:
                    debug(f"{path} <-- {label}")
                    all_sources.add(path)
            elif label_requires_content_hash(label):
                debug(f"IGNORED: {label}")
                ignored_labels.append(label)

        if _ASSERT_ON_IGNORED_FILES and ignored_labels:
            print(
                "ERROR: Found ignored external repository files:",
                file=sys.stderr,
            )
            for label in ignored_labels:
                print(f"  {label}", file=sys.stderr)
            print(
                """
These files are likely generated by a Bazel repository rule which has
no associated content hash file. Due to this, Bazel may regenerate them
semi-randomly in ways that confuse Ninja dependency computations.

To solve this issue, change the build/bazel/scripts/update_workspace.py
script to add corresponding entries to the generated_repositories_inputs
dictionary, to track all input files that the repository rule may
access when it is run.
""",
                file=sys.stderr,
            )
            return 1

        depfile_content = "%s: %s\n" % (
            " ".join(depfile_quote(c) for _, c in file_copies + dir_copies),
            " ".join(depfile_quote(c) for c in sorted(all_sources)),
        )

        if _DEBUG:
            debug("DEPFILE[%s]\n" % depfile_content)

        os.makedirs(os.path.dirname(args.depfile), exist_ok=True)
        with open(args.depfile, "w") as f:
            f.write(depfile_content)

        for f in args.stamp_files:
            with open(f, "w") as f:
                f.write("")

    # Done!
    return 0


if __name__ == "__main__":
    sys.exit(main())
