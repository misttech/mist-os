# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageInfo", "FuchsiaPackageResourcesInfo")
load("//test_utils:py_test_utils.bzl", "PY_TOOLCHAIN_DEPS", "populate_py_test_sh_script")

def _fuchsia_package_checker_test_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    package_info = ctx.attr.package_under_test[FuchsiaPackageInfo]
    meta_far = package_info.meta_far

    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    args = [
        "--far={}".format(sdk.far.short_path),
        "--merkleroot={}".format(sdk.merkleroot.short_path),
        "--meta_far={}".format(meta_far.short_path),
        "--package_name={}".format(ctx.attr.package_name),
    ]

    # apped the components
    args.extend(["--manifests={}".format(m) for m in ctx.attr.manifests])

    runfiles = [
        meta_far,
        sdk.far,
        sdk.merkleroot,
    ]

    # Flatten the list of stripped blobs
    expected_stripped_blobs = [
        blob
        for resources in [r[FuchsiaPackageResourcesInfo].resources for r in ctx.attr.stripped_blobs]
        for blob in resources
    ]

    for blob in expected_stripped_blobs:
        # find the stripeed version of the blob
        for resource in package_info.package_resources:
            if (blob.dest == resource.dest) and blob.src.basename + "_stripped" == resource.src.basename:
                args.append("--blobs={}={}".format(blob.dest, resource.src.short_path))
                runfiles.append(resource.src)
                break

    expected_unstripped_blobs = [
        blob
        for resources in [r[FuchsiaPackageResourcesInfo].resources for r in ctx.attr.unstripped_blobs]
        for blob in resources
    ]

    for blob in expected_unstripped_blobs:
        args.append("--blobs={}={}".format(blob.dest, blob.src.short_path))
        runfiles.append(blob.src)

    populate_py_test_sh_script(ctx, script, ctx.executable._package_checker, args)

    return [
        DefaultInfo(
            executable = script,
            runfiles = ctx.runfiles(
                files = runfiles,
            ).merge(ctx.attr._package_checker[DefaultInfo].default_runfiles),
        ),
    ]

fuchsia_package_checker_test = rule(
    doc = """Validate the generated package.""",
    test = True,
    implementation = _fuchsia_package_checker_test_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "package_under_test": attr.label(
            doc = "Built Package.",
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
        "package_name": attr.string(
            doc = "The expected package name",
            mandatory = True,
        ),
        "manifests": attr.string_list(
            doc = "A list of expected manifests in meta/foo.cm form",
            mandatory = True,
        ),
        "stripped_blobs": attr.label_list(
            doc = """
            A list of package resources which will have been attempted to be
            stripped of debug symbols.
        """,
            mandatory = False,
        ),
        "unstripped_blobs": attr.label_list(
            doc = """
            A list of package resources which will not have been attempted to be
            stripped of debug symbols.
        """,
            mandatory = False,
        ),
        "_package_checker": attr.label(
            default = "//tools:package_checker",
            executable = True,
            cfg = "exec",
        ),
    } | PY_TOOLCHAIN_DEPS,
)
