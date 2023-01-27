# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("//fuchsia/private:providers.bzl", "FuchsiaPackageRepoPathInfo")

def package_repo_path_from_label(label):
    if FuchsiaPackageRepoPathInfo in label:
        repo_path = label[FuchsiaPackageRepoPathInfo].path
        return repo_path if repo_path != "" else None
    return None

def publish_package(ctx, pm, repo_path, package_manifests):
    """ Will publish the list of package manifests to the server at repo_path

    Args:
          ctx: The rule context
          pm: The pm tool
          repo_path: The path to the package repository
          package_manifests: A list of packages that should be published.

    Returns:
        The stamp file indicating that the publishing was succesful
    """

    stamp_file = ctx.actions.declare_file(ctx.label.name + "_publish.stamp")

    list_of_packages = ctx.actions.declare_file(ctx.label.name + "_packages.list")
    ctx.actions.write(
        output = list_of_packages,
        content = "\n".join([p.path for p in package_manifests]),
    )

    # Wrap the publishing in a script which will write to a stamp file. We need
    # to run this in a script instead of an action because `pm publish` does not
    # write to a file and thus Bazel will not run it and we need to write to a
    # repository outside of the Bazel sandbox.

    content = """#!/bin/bash
    if [[ ! -d "{repo}" ]]; then
        echo >&2 "WARNING: no repository in {repo}, creating it!"
        {pm} newrepo -repo {repo} || exit 1
    fi
    {pm} publish -n -lp -f {packages} -repo {repo} || exit 1

    echo 'SUCCESS' > {stampfile}
    """.format(pm = pm.path, packages = list_of_packages.path, repo = repo_path, stampfile = stamp_file.path)

    publish_script = ctx.actions.declare_file(ctx.label.name + "_publish_package.sh")
    ctx.actions.write(
        output = publish_script,
        content = content,
        is_executable = True,
    )

    ctx.actions.run(
        executable = publish_script,
        inputs = package_manifests + [list_of_packages],
        outputs = [
            stamp_file,
        ],
        mnemonic = "FuchsiaPmPublish",
        progress_message = "Publishing package for %{target.label}",
    )

    return stamp_file
