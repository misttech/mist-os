# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""The googletest_repository() repository rule"""

def _googletest_repository_impl(repo_ctx):
    """Create a @com_google_googletest repository that supports Fuchsia."""
    workspace_dir = str(repo_ctx.workspace_root)

    # IMPORTANT: keep this function in sync with the computation of
    # generated_repository_inputs['com_google_googletest'] in
    # //build/bazel/update-workspace.py.
    if hasattr(repo_ctx.attr, "content_hash_file"):
        repo_ctx.path(workspace_dir + "/" + repo_ctx.attr.content_hash_file)

    # This uses a git bundle to ensure that we can always work from a
    # Jiri-managed clone of //third_party/googletest/src/. This is more reliable
    # than the previous approach that relied on patching.
    repo_ctx.execute(
        [
            repo_ctx.path(workspace_dir + "/build/bazel/scripts/git-clone-then-apply-bundle.py"),
            "--dst-dir",
            ".",
            "--git-url",
            repo_ctx.path(workspace_dir + "/third_party/googletest/src"),
            "--git-bundle",
            repo_ctx.path(workspace_dir + "/build/bazel/patches/googletest/fuchsia-support.bundle"),
            "--git-bundle-head",
            "fuchsia-support",
        ],
        quiet = False,  # False for debugging.
    )

googletest_repository = repository_rule(
    implementation = _googletest_repository_impl,
    doc = "A repository rule used to create a googletest repository that " +
          "properly supports Fuchsia through local patching.",
    attrs = {
        "content_hash_file": attr.string(
            doc = "Path to content hash file for this repository, relative to workspace root.",
            mandatory = False,
        ),
    },
)
