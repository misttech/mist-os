# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("//fuchsia/private/workflows:providers.bzl", "FuchsiaProductBundleInfo")
load("//fuchsia/private/workflows:fuchsia_product_bundle_tasks.bzl", "fuchsia_task_fetch_product_bundle", "fuchsia_task_remove_product_bundle")
load(
    "//fuchsia/private/workflows:fuchsia_package_repository_tasks.bzl",
    "fuchsia_task_repository_delete",
)
load("//fuchsia/private/workflows:fuchsia_task_verbs.bzl", "make_help_executable", "verbs")

def fuchsia_remote_product_bundle(
        name,
        product_name,
        version = None,
        repository = None,
        **kwargs):
    """
    Describes a product bundle which is not built locally and the tasks which fetch it.

    The following tasks will be created:
    - name.fetch: Fetches the product bundle
    - name.remove: Removes the product bundle
    - name.delete_repo: Deletes the repository associated with this bundle

    Args:
        name: The target name.
        product_name: The name of the product to fetch.
        version: A specific version to fetch. Defaults to the sdk version.
        repository: The name of the repository to host the product bundle's packages.
        **kwargs: Extra attributes to pass along to the build rule.
    """
    _fuchsia_remote_product_bundle(
        name = name,
        product_name = product_name,
        version = version,
        repository = repository,
        **kwargs
    )

    fuchsia_task_fetch_product_bundle(
        name = verbs.fetch(name),
        product_bundle = name,
    )

    fuchsia_task_remove_product_bundle(
        name = verbs.remove(name),
        product_bundle = name,
    )

    fuchsia_task_repository_delete(
        name = verbs.delete_repo(name),
        repository_name = repository or product_name,
        preserve_contents = True,
    )

def _fuchsia_remote_product_bundle_impl(ctx):
    return [
        DefaultInfo(executable = make_help_executable(
            ctx,
            {
                verbs.fetch: "Fetches the product bundle",
                verbs.remove: "Removes the product bundle",
                verbs.delete_repo: "Deletes the repository associated with this bundle",
            },
        )),
        FuchsiaProductBundleInfo(
            is_remote = True,
            product_bundle = ctx.attr.product_name,
            product_name = ctx.attr.product_name,
            version = ctx.attr.version,
            repository = ctx.attr.repository,
        ),
    ]

_fuchsia_remote_product_bundle = rule(
    implementation = _fuchsia_remote_product_bundle_impl,
    doc = "A rule describing a remote product bundle.",
    attrs = {
        "product_name": attr.string(
            doc = "The name of the product to download",
            mandatory = True,
        ),
        "version": attr.string(
            doc = """
            The version of the product bundle. If not supplied will default to
            the version specified by the SDK""",
        ),
        "repository": attr.string(
            doc = """
            The name of the repository to host the packages in the product bundle.
            If not provided the product_name will be used.
            """,
        ),
    },
    executable = True,
)
