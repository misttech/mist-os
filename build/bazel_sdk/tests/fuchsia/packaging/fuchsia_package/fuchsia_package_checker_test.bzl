# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load("//test_utils:py_test_utils.bzl", "PY_TOOLCHAIN_DEPS", "populate_py_test_sh_script")

def _fuchsia_package_checker_test_impl(ctx):
    sdk = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    package_info = ctx.attr.package_under_test[FuchsiaPackageInfo]
    meta_far = package_info.meta_far

    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    args = [
        "--far={}".format(sdk.far.short_path),
        "--ffx={}".format(sdk.ffx.short_path),
        "--meta_far={}".format(meta_far.short_path),
        "--package_name={}".format(ctx.attr.package_name),
    ]

    runfiles = [
        meta_far,
        sdk.far,
        sdk.ffx,
    ]

    # Find all of our blobs
    dest_to_resource = {}
    for resource in package_info.package_resources:
        dest_to_resource[resource.dest] = resource

    for (dest, name) in ctx.attr.expected_blobs_to_file_names.items():
        if dest in dest_to_resource:
            resource = dest_to_resource[dest]
            src_path = resource.src.short_path
            if src_path.endswith(name):
                args.append("--blobs={}={}".format(dest, resource.src.short_path))
                runfiles.append(resource.src)
            else:
                fail("Expected blob {} does not match expected filename {}".format(dest, name))
        else:
            fail("Expected blob {} not in resources {}".format(dest, dest_to_resource))

    # apped the components
    args.extend(["--manifests={}".format(m) for m in ctx.attr.manifests])

    # append the bind bytecode
    if ctx.attr.bind_bytecode:
        args.append("--bind_bytecode={}".format(ctx.attr.bind_bytecode))

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
        "bind_bytecode": attr.string(
            doc = "A path to the bind bytecode for the driver in meta/bind/foo.bindbc form",
            mandatory = False,
        ),
        "expected_blobs_to_file_names": attr.string_dict(
            doc = """The list of blobs we expect in the package.

            The key is the install location and the value is the local file name.
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
