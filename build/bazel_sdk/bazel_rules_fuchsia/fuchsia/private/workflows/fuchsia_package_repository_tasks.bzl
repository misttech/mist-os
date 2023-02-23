# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""ffx package repository invokations as workflow tasks."""

load(":fuchsia_task_ffx.bzl", "ffx_task_rule")
load("//fuchsia/private:providers.bzl", "FuchsiaLocalPackageRepositoryInfo", "FuchsiaProductBundleInfo")
load("//fuchsia/private/workflows:fuchsia_shell_task.bzl", "shell_task_rule")

def _repo_task(ctx, make_shell_task, use_ffx = True, args = []):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

    tool = sdk.ffx if use_ffx else sdk.pm
    command = [
        "TOOL=$(readlink -f %s)" % tool.short_path,
        "&&",
        "cd",
        "${BUILD_WORKSPACE_DIRECTORY}",
        "&&",
        "$TOOL",
    ]

    return make_shell_task(
        command = command + args,
        runfiles = [tool],
    )

def _fuchsia_task_repository_create_impl(ctx, make_fuchsia_task):
    return _repo_task(
        ctx,
        make_fuchsia_task,
        use_ffx = False,
        args = [
            "newrepo",
            "-vt",
            "-repo",
            ctx.attr.repository[FuchsiaLocalPackageRepositoryInfo].repo_path,
        ],
    )

(
    _fuchsia_task_repository_create,
    _fuchsia_task_repository_create_for_test,
    fuchsia_task_repository_create,
) = shell_task_rule(
    implementation = _fuchsia_task_repository_create_impl,
    doc = """Creates a package server.""",
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "repository": attr.label(
            doc = "The repository that is being controlled",
            providers = [[FuchsiaLocalPackageRepositoryInfo]],
            mandatory = True,
        ),
    },
)

def _fuchsia_task_repository_add_from_pm_impl(ctx, make_fuchsia_task):
    repo = ctx.attr.repository[FuchsiaLocalPackageRepositoryInfo]
    return _repo_task(
        ctx,
        make_fuchsia_task,
        use_ffx = True,
        args = [
            "repository",
            "add-from-pm",
            "--repository",
            repo.repo_name,
            repo.repo_path,
        ],
    )

(
    _fuchsia_task_repository_add_from_pm,
    _fuchsia_task_repository_add_from_pm_for_test,
    fuchsia_task_repository_add_from_pm,
) = shell_task_rule(
    doc = """Adds a pm based repo to ffx.""",
    implementation = _fuchsia_task_repository_add_from_pm_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "repository": attr.label(
            doc = "The repository that is being controlled",
            providers = [[FuchsiaLocalPackageRepositoryInfo]],
            mandatory = True,
        ),
    },
)

#TODO: Pipe this through some processing to do ffx --machine JSON repository list and get the path
def _fuchsia_task_repository_delete_impl(ctx, make_shell_task):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    command = [
        ctx.attr._clean_repo,
        "--ffx",
        sdk.ffx,
        "--name",
        ctx.attr.repository_name,
    ]
    if ctx.attr.path:
        command.extend([
            "--fallback_path",
            ctx.attr.path,
        ])

    if ctx.attr.preserve_contents:
        command.append("--no-delete_contents")

    return make_shell_task(
        command = command,
    )

(
    _fuchsia_task_repository_delete,
    _fuchsia_task_repository_delete_for_test,
    fuchsia_task_repository_delete,
) = shell_task_rule(
    implementation = _fuchsia_task_repository_delete_impl,
    doc = """deletes a package server.""",
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "repository_name": attr.string(
            doc = "The repository to delete",
            mandatory = True,
        ),
        "path": attr.string(
            doc = "The path to this repository",
            mandatory = False,
        ),
        "preserve_contents": attr.bool(
            doc = "If true, the contents will not be deleted from disk",
            mandatory = False,
            default = False,
        ),
        "_clean_repo": attr.label(
            default = "//fuchsia/tools:clean_repo",
            doc = "The tool to remove the repo.",
            executable = True,
            cfg = "target",
        ),
    },
)

def _fuchsia_package_repository_make_default_impl(ctx, _make_ffx_task):
    return _make_ffx_task(
        prepend_args = [
            "-c",
            "'ffx_repository=true'",
            "repository",
            "default",
            "set",
            ctx.attr.repository[FuchsiaLocalPackageRepositoryInfo].repo_name,
        ],
    )

(
    _fuchsia_package_repository_make_default,
    _fuchsia_package_repository_make_default_for_test,
    fuchsia_package_repository_make_default,
) = ffx_task_rule(
    doc = """Makes a package server default.""",
    implementation = _fuchsia_package_repository_make_default_impl,
    attrs = {
        "repository": attr.label(
            doc = "The repository that is being controlled",
            providers = [[FuchsiaLocalPackageRepositoryInfo]],
            mandatory = True,
        ),
    },
)

def fuchsia_task_repository_register_with_default_target(*, name, testonly = True, **kwargs):
    _fuchsia_task_repository_register_with_default_target(
        name = name,
        testonly = testonly,
        **kwargs
    )

def _fuchsia_task_repository_register_with_default_target_impl(ctx, _make_ffx_task):
    if FuchsiaLocalPackageRepositoryInfo in ctx.attr.repository:
        repo = ctx.attr.repository[FuchsiaLocalPackageRepositoryInfo].repo_name
    elif FuchsiaProductBundleInfo in ctx.attr.repository:
        repo = ctx.attr.repository[FuchsiaProductBundleInfo].repository
    else:
        fail("Only product bundles and local repositories are supported at this time.")

    return _make_ffx_task(
        prepend_args = [
            "target",
            "repository",
            "register",
            "--repository",
            repo,
        ],
    )

_fuchsia_task_repository_register_with_default_target = ffx_task_rule(
    doc = """Registers the repo with the default target.""",
    implementation = _fuchsia_task_repository_register_with_default_target_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "repository": attr.label(
            doc = "The repository that is being controlled",
            providers = [[FuchsiaLocalPackageRepositoryInfo], [FuchsiaProductBundleInfo]],
            mandatory = True,
        ),
    },
)
