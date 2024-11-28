# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("//fuchsia/private:fuchsia_prebuilt_package.bzl", "fuchsia_prebuilt_package")
load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS")

def _gen_android_starnix_container_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]

    _hal_files = []
    for hal in ctx.attr.hals:
        _hal_files += hal[FuchsiaPackageInfo].files

    _hal_package_manifests = [
        hal[FuchsiaPackageInfo].package_manifest
        for hal in ctx.attr.hals
    ]
    _package_inputs = [
        ctx.attr.base[FuchsiaPackageInfo].package_manifest,
        ctx.file.system,
    ] + ctx.files.base_files + _hal_files + ctx.attr.base[FuchsiaPackageInfo].files

    _container_manifest = ctx.actions.declare_file(ctx.label.name + "_out/package_manifest.json")

    # Bazel doesn't allow declaring output files under a declared directory, so
    # we can't declare the output directory and the manifest file as output at
    # the same time. To work around that, we explicitly declare siblings of the
    # manifest as outputs. This is coupled with the implementation of
    # gen_android_starnix_container.
    meta_far = ctx.actions.declare_file("meta.far", sibling = _container_manifest)
    _outputs = [
        _container_manifest,
        ctx.actions.declare_directory("meta", sibling = _container_manifest),
        meta_far,
        ctx.actions.declare_directory("odm", sibling = _container_manifest),
        ctx.actions.declare_directory("system", sibling = _container_manifest),
    ]

    _args = [
        "--name",
        ctx.attr.package_name,
        "--outdir",
        _container_manifest.dirname,
        "--base",
        ctx.attr.base[FuchsiaPackageInfo].package_manifest.path,
        "--system",
        ctx.file.system.path,
    ]
    for hal in _hal_package_manifests:
        _args += [
            "--hal",
            hal.path,
        ]

    if ctx.file.vendor:
        _package_inputs += [ctx.file.vendor]
        _args += [
            "--vendor",
            ctx.file.vendor.path,
        ]
        _outputs += [
            ctx.actions.declare_directory("vendor", sibling = _container_manifest),
        ]

    ctx.actions.run(
        executable = sdk.gen_android_starnix_container,
        arguments = _args,
        inputs = _package_inputs,
        outputs = _outputs,
        **LOCAL_ONLY_ACTION_KWARGS
    )

    return [
        DefaultInfo(files = depset(direct = _outputs)),
        FuchsiaPackageInfo(
            package_manifest = _container_manifest,
            files = _package_inputs + _outputs,
            meta_far = meta_far,
            package_resources = [],
        ),
    ]

_gen_android_starnix_container_internal = rule(
    doc = "Construct a starnix container that can include an Android system and HALs.",
    implementation = _gen_android_starnix_container_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "base": attr.label(
            doc = "Path to package containing base resources to include",
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
        "base_files": attr.label_list(
            doc = "Files referenced by base package manifest",
            allow_files = True,
        ),
        "package_name": attr.string(
            doc = "Name of the starnix container package",
        ),
        "system": attr.label(
            doc = "The Android system image",
            allow_single_file = True,
            mandatory = True,
        ),
        "vendor": attr.label(
            doc = "The Android vendor image",
            allow_single_file = True,
        ),
        "hals": attr.label_list(
            doc = "HALs to include in this container. List of fuchsia_prebuilt_package labels or far files",
            allow_files = True,
        ),
    },
)

def fuchsia_gen_android_starnix_container(name, base, system, base_files = [], package_name = None, hals = [], vendor = None):
    _gen_android_starnix_container_internal(
        name = name,
        package_name = package_name if package_name else name,
        base = base,
        base_files = base_files,
        hals = hals,
        system = system,
        vendor = vendor,
    )
