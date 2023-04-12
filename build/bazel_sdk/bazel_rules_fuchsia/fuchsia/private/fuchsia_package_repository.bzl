# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Build rule for Fuchsia repository."""

load(
    ":providers.bzl",
    "FuchsiaPackageGroupInfo",
    "FuchsiaPackageInfo",
    "FuchsiaPackageRepoInfo",
)

def _fuchsia_package_repository_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    repo_name = ctx.attr.repo_name or ctx.label.name
    packages = []
    package_deps = []
    for dep in ctx.attr.deps:
        if FuchsiaPackageInfo in dep:
            packages.append(dep[FuchsiaPackageInfo].package_manifest.path)
            package_deps.extend(dep[FuchsiaPackageInfo].files)
            package_deps.append(dep[FuchsiaPackageInfo].package_manifest)

    list_of_packages = ctx.actions.declare_file("%s_packages.list" % repo_name)

    ctx.actions.write(
        output = list_of_packages,
        content = "\n".join(packages),
    )

    # Publish the packages
    repo_dir = ctx.actions.declare_directory("%s.repo" % repo_name)
    ctx.actions.run(
        executable = sdk.ffx,
        arguments = ["repository", "publish", "--clean", repo_dir.path],
        inputs = depset(package_deps + [list_of_packages]),
        outputs = [repo_dir],
        mnemonic = "FuchsiaFfxRepositoryPublish",
        progress_message = "Publishing package repository %{label}",
    )

    return [
        DefaultInfo(files = depset([repo_dir])),
        FuchsiaPackageRepoInfo(
            packages = packages,
            repo_dir = repo_dir.path,
            blobs = package_deps,
        ),
    ]

fuchsia_package_repository = rule(
    doc = """
A Fuchsia TUF package repository as created by the 'pm' tool and used by 'ffx repository'.
""",
    implementation = _fuchsia_package_repository_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    provides = [FuchsiaPackageRepoInfo],
    attrs = {
        "deps": attr.label_list(
            doc = "Fuchsia package and package groups to include in this repository.",
            providers = [
                [FuchsiaPackageInfo],
                [FuchsiaPackageGroupInfo],
            ],
        ),
        "repo_name": attr.string(
            doc = "The repository name, defaults to the rule name",
            mandatory = False,
        ),
    },
)
