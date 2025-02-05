# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

def _boringssl_repository_impl(repo_ctx):
    """Create a @boringssl repository."""

    workspace_dir = str(repo_ctx.workspace_root)
    dest_dir = repo_ctx.path(".")
    src_dir = repo_ctx.path(workspace_dir + "/third_party/boringssl")

    # IMPORTANT: keep this function in sync with the computation of
    # generated_repository_inputs['boringssl'] in
    # //build/bazel/workspace_utils.py.
    if hasattr(repo_ctx.attr, "content_hash_file"):
        repo_ctx.path(workspace_dir + "/" + repo_ctx.attr.content_hash_file)

    # Link the contents of the repo into the bazel sandbox. We cannot use a
    # local_repository here because we need to execute the python script below
    # which generates the build file contents.
    repo_ctx.execute(
        [
            repo_ctx.path(workspace_dir + "/build/bazel/scripts/hardlink-directory.py"),
            "--fuchsia-dir",
            workspace_dir,
            src_dir,
            dest_dir,
        ],
        quiet = False,  # False for debugging.
    )

    # Copy the generated files into the workspace root
    generated_files = [
        "BUILD.generated.bzl",
        "BUILD.generated_tests.bzl",
    ]

    for generated_file in generated_files:
        content = repo_ctx.read(
            repo_ctx.path(workspace_dir + "/third_party/boringssl/" + generated_file),
        )
        repo_ctx.file(
            generated_file,
            content = content,
            executable = False,
        )

    # Add a BUILD file which exposes the cc_library target.
    repo_ctx.file("BUILD.bazel", content = repo_ctx.read(
        repo_ctx.path(workspace_dir + "/build/bazel/local_repositories/boringssl/BUILD.boringssl"),
    ), executable = False)

boringssl_repository = repository_rule(
    implementation = _boringssl_repository_impl,
    doc = "A repository rule used to create a boringssl repository that " +
          "has build files generated for Bazel.",
    attrs = {
        "content_hash_file": attr.string(
            doc = "Path to content hash file for this repository, relative to workspace root.",
            mandatory = False,
        ),
    },
)
