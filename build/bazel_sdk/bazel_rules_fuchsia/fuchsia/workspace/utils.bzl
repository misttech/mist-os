# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Common utilities needed by @fuchsia_workspace rules."""

def normalize_os(ctx):
    # On osx os.name => "mac os x".
    return ctx.os.name.split(" ")[0]

def symlink_or_copy(ctx, copy_content_strategy, files_to_copy):
    """ Symlink, hardlink or copy files from one location to another, replicating the relative directory structure

    Args:
        ctx: repository context
        copy_content_strategy: "copy" or "symlink"
        files_to_copy: a dict of string to list, where the key is a string with the root path and the value is a list of string paths relative to the root

    """
    if copy_content_strategy == "copy":
        per_dest_dir = {}
        for root in files_to_copy.keys():
            for file in files_to_copy[root]:
                dest = ctx.path(file)
                dest_dir = str(dest.dirname)
                origin_file = str(ctx.path("%s/%s" % (root, file)))
                if dest_dir in per_dest_dir:
                    for other in per_dest_dir[dest_dir]:
                        if ctx.path(other).basename == dest.basename:
                            fail("File is specified in two different places: in %s and in %s" % (other, origin_file))
                    per_dest_dir[dest_dir].append(origin_file)
                else:
                    per_dest_dir[dest_dir] = [origin_file]

        host_os = normalize_os(ctx)
        try_hardlink = host_os == "linux"
        for dest_dir in per_dest_dir.keys():
            ctx.execute(["mkdir", "-p", dest_dir], quiet = False)

            # for performance reasons, if a single hardlink operation fails, we switch to regular copy for
            # all the remaining destination directories
            if try_hardlink:
                result = ctx.execute(["cp", "-l", "--no-clobber"] + per_dest_dir[dest_dir] + [dest_dir], quiet = True)
                if result.return_code != 0:
                    try_hardlink = False

            # this is not an "else" intentionally, since we want to execute regular copy if we couldn't hardlink
            if not try_hardlink:
                # Note: -n is the same as --no-clobber but is supported on MacOS.
                command = ["cp", "-n"] + per_dest_dir[dest_dir] + [dest_dir]
                result = ctx.execute(command, quiet = False)
                if result.return_code != 0:
                    fail("Cannot copy files (%s):\n     %s" % (str(result.return_code), " ".join(command)))

    elif copy_content_strategy == "symlink":
        for root in files_to_copy.keys():
            for file in files_to_copy[root]:
                ctx.symlink(ctx.path("%s/%s" % (root, file)), ctx.path(file))
    else:
        fail("Invalid value of copy_content_strategy argument: %s" % copy_content_strategy)

def workspace_path(repo_ctx, local_path):
    """Resolve a local path relative to the main workspace directory.

    Args:
      repo_ctx: A repository_ctx instance.

      local_path: Either a Path object, or a relative or absolute path string.
          If absolute, the path is  returned as is. If relative, it is resolved
          relative to the main workspace's directory.

    Returns:
      An absolute path string.
    """
    local_path = str(local_path)
    if local_path.startswith("/"):
        return local_path

    # Using repo_ctx.workspace_root will only work when Bazel 6.0 ships.
    # See see https://github.com/bazelbuild/bazel/issues/16042.
    #
    # The following is a work-around that will fail is the main project
    # uses a WORKSPACE, instead of WORKSPACE.bazel, file. Unfortunately,
    # Bazel will complain directly when trying to resolve a Label() to
    # a non-existing file so it is not possible to check for both cases
    # with Path.exist.
    bazel_version_major = native.bazel_version.split(".")[0]
    if int(bazel_version_major) >= 6:
        workspace_root = repo_ctx.workspace_root
    else:
        workspace_root = repo_ctx.path(Label("@//:WORKSPACE.bazel")).dirname

    return "%s/%s" % (workspace_root, local_path)
