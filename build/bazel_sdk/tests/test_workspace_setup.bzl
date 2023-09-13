# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Provides a set of convenience methods to set up the test workspace in fuchsia.git.

These methods are only useful in the fuchsia.git repository. They will fail if
they are used in any other repqsitory since they are using fuchsia specific
source layouts.
"""

_BUILD_DIR_NAME = "fx-build-dir"

def setup_test_workspace(name, fuchsia_root):
    """Creates a workspace which provides utilities for setting up the test workspace"""
    build_dir_wrapper_name = name + "-build-dir-wrapper"
    _build_dir_wrapper(name = build_dir_wrapper_name)
    _setup_test_workspace(
        name = name,
        build_dir = "@{}//:{}".format(build_dir_wrapper_name, _BUILD_DIR_NAME),
        fuchsia_root = fuchsia_root,
    )

def _workspace_path(repo_ctx, local_path):
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

    return "%s/%s" % (repo_ctx.workspace_root, local_path)

def _build_dir_wrapper_impl(ctx):
    ctx.file("WORKSPACE.bazel", content = "")
    ctx.file("BUILD.bazel", content = 'exports_files(srcs = ["{}"])'.format(_BUILD_DIR_NAME))

    local_build_dir = _workspace_path(ctx, "../../../.fx-build-dir")
    if ctx.path(local_build_dir).exists:
        # Create a symlink ot the .fx-build-dir. Doing this will allow us to pass
        # the symlink in as a tracked file. This allows us to invalidate the repo
        # when the build-dir changes.
        ctx.symlink(local_build_dir, _BUILD_DIR_NAME)
    else:
        # Infrastructure builds don't have a build dir but they do hardcode the
        # path to "out/not-default" so just create a file that mimics that.
        ctx.file(_BUILD_DIR_NAME, content = "out/not-default")

_build_dir_wrapper = repository_rule(
    implementation = _build_dir_wrapper_impl,
    doc = "Create a repository rule which will track the build-dir",
)

def _setup_test_workspace_impl(ctx):
    ctx.file("WORKSPACE.bazel", content = "")
    ctx.file("BUILD.bazel", content = "")

    fuchsia_root = ctx.attr.fuchsia_root.removesuffix("/")

    # We are tracking the build dir as in input file here so when it changes
    # this method will run again which changes our local_repository path. This
    # allows us to change our build dir and pick up the changes.
    build_dir = ctx.read(ctx.attr.build_dir).rstrip()

    if build_dir == "out/not-default":
        # This is an infra build don't try to use the local_path
        fuchsia_sdk_path = ""
    else:
        # Read the build api to figure out where the bazel sdk is
        relative_build_dir = fuchsia_root + "/" + build_dir
        sdk_info_path = ctx.path(
            _workspace_path(
                ctx,
                relative_build_dir,
            ),
        ).get_child("bazel_sdk_info.json")
        sdk_build_api = json.decode(
            ctx.read(sdk_info_path),
        )
        fuchsia_sdk_path = relative_build_dir + "/" + sdk_build_api[0]["location"]

    if ctx.os.name.startswith("mac"):
        os = "mac"
    else:
        os = "linux"

    if ctx.os.arch.startswith("x86_64") or ctx.os.arch.startswith("amd64"):
        arch = "x64"
    else:
        arch = "arm64"

    defs_template = """
load(
    "@fuchsia_workspace//fuchsia:deps.bzl",
    "fuchsia_sdk_repository",
)

def local_fuchsia_sdk_repository():
    local_path = "{fuchsia_sdk_path}"
    if local_path == "":
        # This is an infra build, rely on the repo overrides
        fuchsia_sdk_repository(
            name = "fuchsia_sdk"
        )
    else:
        native.local_repository(
            name = "fuchsia_sdk",
            path = local_path,
        )

CLANG_PATH = "../../../prebuilt/third_party/clang/{os}-{arch}"
""".format(
        fuchsia_sdk_path = fuchsia_sdk_path,
        os = os,
        arch = arch,
    )

    ctx.file("defs.bzl", content = defs_template)

_setup_test_workspace = repository_rule(
    implementation = _setup_test_workspace_impl,
    attrs = {
        "build_dir": attr.label(
            allow_single_file = True,
        ),
        "fuchsia_root": attr.string(),
    },
    local = True,
)
