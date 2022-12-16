# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Misc utilities to deal with behavior changes between Bazel versions."""

# See https://github.com/aspect-build/bazel-lib/blob/main/lib/private/utils.bzl#L149
is_bazel6_or_greater = "apple_binary" not in dir(native)

def actions_symlink_file_or_directory(ctx, dest_path, src_path):
    """Create an action graph command that creates a symlink.

    This takes care of a slight difference between Bazel 6 and previous versions
    when symlinking directories.

    Args:
       ctx: A rule context object.
       dest_path: Destination path where the symlink will be created.
       src_path: The source File reference, this can point to either a file or directory.
    Returns:
       The File object for the created symlink.
    """

    # Note: Starting with Bazel6, repository_ctx.symlink() will error if the `output`
    # and `target_file` arguments are not both files or both directories, so use
    # declare_directory(). Unfortunately, doing the same in Bazel 5.4 results in
    # an error.
    if is_bazel6_or_greater and src_path.is_directory:
        dest = ctx.actions.declare_directory(dest_path)
    else:
        dest = ctx.actions.declare_file(dest_path)

    ctx.actions.symlink(output = dest, target_file = src_path)
    return dest
