# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
A repository rule `fuchsia_icu_config_repository` generates an external repo
that contains git commit ID information about the third party ICU repositories
contained in @//third_party/icu.

Defines a constant `icu_flavors` which is a struct containing these elements:

- `default_git_commit`(string): the detected git commit ID for
   `@//:third_party/icu_default`
- `latest_git_commit`(string): the detected git commit ID for
   `@//:third_party/icu_latest`
- `stable_git_commit`(string): the detected git commit ID for
   `@//:third_party/icu_stable`

This struct can be ingested by main build rules by using:

In WORKSPACE.bazel:

```
load ("//:bazel/icu/repository_rules.bzl:", "fuchsia_icu_config_repository")

fuchsia_icu_config_repository(name = "fuchsia_icu_config")
```

in BUILD files:

```
load("@fuchsia_icu_config//:constants.bzl", "icu_flavors")
```

"""

load("@bazel_skylib//lib:paths.bzl", "paths")

def _fuchsia_icu_config_impl(repo_ctx):
    workspace_root = str(repo_ctx.path(Label("@//:WORKSPACE.bazel")).dirname)

    # Ensure this repository is regenerated any time the content hash file
    # changes. Creating a content hash file in update-workspace.py allows
    # grabbing the correct path to the real .git/HEAD when submodules are
    # used, which is harder to use in Starlark than in Python.
    if hasattr(repo_ctx.attr, "content_hash_file"):
        repo_ctx.path(Label("@//:" + repo_ctx.attr.content_hash_file))

    script = repo_ctx.path(Label("//:build/icu/gen-git-head-commit-id.sh"))
    cmd = " ".join([
        str(script),
        "bzl",
        paths.join(workspace_root, "third_party/icu/default"),
        paths.join(workspace_root, "third_party/icu/stable"),
        paths.join(workspace_root, "third_party/icu/latest"),
        ">",
        "constants.bzl",
    ])

    # $PWD for this script is this repository's root, not the main repo's root.
    result = repo_ctx.execute([
        "/bin/bash",
        "-c",
        cmd,
    ])
    if result.return_code:
        fail("script exited with error code: {}, stderr: {}".format(
            result.return_code,
            result.stderr,
        ))
    repo_ctx.file("WORKSPACE.bazel", """# DO NOT EDIT! Automatically generated.
workspace(name = "fuchsia_icu_config")
""")
    repo_ctx.file("BUILD.bazel", """# DO NOT EDIT! Automatically generated.
exports_files(glob(["**/*"]))""")

fuchsia_icu_config_repository = repository_rule(
    implementation = _fuchsia_icu_config_impl,
    doc = "Create a repository that contains ICU configuration information in its //:constants.bzl file.",
    attrs = {
        "content_hash_file": attr.string(
            doc = "Path to content hash file for this repository, relative to workspace root.",
            mandatory = False,
        ),
    },
)
