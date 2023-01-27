# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Build rule for Fuchsia repository

load(
    ":providers.bzl",
    "FuchsiaLocalPackageRepositoryInfo",
    "FuchsiaPackageGroupInfo",
    "FuchsiaPackageInfo",
    "FuchsiaPackageRepoInfo",
)
load(
    "//fuchsia/private/workflows:fuchsia_package_repository_tasks.bzl",
    "fuchsia_package_repository_make_default",
    "fuchsia_task_repository_add_from_pm",
    "fuchsia_task_repository_create",
    "fuchsia_task_repository_delete",
)
load(
    "//fuchsia/private/workflows:fuchsia_workflow.bzl",
    "fuchsia_workflow",
)
load(
    "//fuchsia/private/workflows:fuchsia_task_verbs.bzl",
    "make_help_executable",
    "verbs",
)

def fuchsia_local_package_repository(
        name,
        repo_name,
        repo_path,
        **kwargs):
    """
    Describes a local fuchsia repository and the tasks which control it.

    The following tasks will be created:
    - name.delete: Deletes the repo
    - name.create: Creates the repo
    - name.make_default: Makes the repo the default.

    Args:
        name: The target name.
        repo_name: The name of the repository
        repo_path: The path of the repository.
        **kwargs: Extra attributes to pass along to the build rule.
    """

    _fuchsia_local_package_repository(
        name = name,
        repo_name = repo_name,
        repo_path = repo_path,
        **kwargs
    )

    fuchsia_task_repository_delete(
        name = verbs.delete(name),
        repository_name = repo_name,
        path = repo_path,
        **kwargs
    )

    fuchsia_task_repository_create(
        name = name + "_create_as_pm",
        repository = name,
        **kwargs
    )

    fuchsia_task_repository_add_from_pm(
        name = name + "_create_add_from_pm",
        repository = name,
        **kwargs
    )

    fuchsia_package_repository_make_default(
        name = verbs.make_default(name),
        repository = name,
        **kwargs
    )

    fuchsia_workflow(
        name = verbs.create(name),
        sequence = [
            name + "_create_as_pm",
            name + "_create_add_from_pm",
        ],
        **kwargs
    )

def _fuchsia_local_package_repository_impl(ctx):
    return [
        DefaultInfo(executable = make_help_executable(
            ctx,
            {
                verbs.delete: "Deletes the repo",
                verbs.create: "Creates the repo",
                verbs.make_default: "Make the repo the default",
            },
        )),
        FuchsiaLocalPackageRepositoryInfo(
            repo_name = ctx.attr.repo_name,
            repo_path = ctx.attr.repo_path,
        ),
    ]

_fuchsia_local_package_repository = rule(
    implementation = _fuchsia_local_package_repository_impl,
    doc = "A rule which describes a fuchsia package repository on a local machine",
    attrs = {
        "repo_name": attr.string(
            doc = "The name of the repository",
            mandatory = True,
        ),
        "repo_path": attr.string(
            doc = """
            The path to the repository on disk. If path is relative it will be
            created relative to the workspace root.
            """,
            mandatory = True,
        ),
    },
    executable = True,
)
