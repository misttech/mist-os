# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@bazel_skylib//lib:paths.bzl", "paths")
load("@fuchsia_sdk//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load("//test_utils:py_test_utils.bzl", "PY_TOOLCHAIN_DEPS", "create_python3_shell_wrapper_provider")
load("@rules_fuchsia//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

def _verify_package_resources(package_info, attr):
    """ Verifies `package_info` contains the resources specified by various `attr` entries.

    Returns a tuple of args and runfiles for expected blobs.
    """
    args = []
    runfiles = []

    # Find all of our blobs
    dest_to_resource = {}
    package_resources = package_info.package_resources or []
    for resource in package_resources:
        dest_to_resource[resource.dest] = resource

    # The resources in the package should be exactly those listed below. The expected resource name
    # is provided for the first. The rest are in meta/ and only the destination is provided. For
    # those, the resource filename matches the destination except for `structured_config_files`.
    # * expected_blobs_to_file_names
    # * manifests
    # * structured_config_files
    # * bind_bytecode
    # * "meta/package"
    expected_meta_resource_dests = attr.manifests + attr.structured_config_files + ["meta/package"]
    if attr.bind_bytecode:
        expected_meta_resource_dests.append(attr.bind_bytecode)
    num_package_resources = len(dest_to_resource)
    num_expected_resources = (len(attr.expected_blobs_to_file_names.items()) +
                              len(expected_meta_resource_dests))
    if num_package_resources != num_expected_resources:
        if attr.expected_generated_blobs:
            # The contents of `fuchsia_gen_android_starnix_container` packages are significantly
            # different from other packages.
            # TODO(https://fxbug.dev/382485650): Determine whether they are correct and how to
            # perform similar validation.
            print("Skipping resource validation for `expected_generated_blobs` test.")
        else:
            fail("Unexpected number of resources in package: {} instead of {}".format(
                num_package_resources,
                num_expected_resources,
            ))

    for (dest, name) in attr.expected_blobs_to_file_names.items():
        if dest in dest_to_resource:
            resource = dest_to_resource[dest]
            src_path = resource.src.short_path
            if src_path.endswith(name):
                args.append("--blobs={}={}".format(dest, resource.src.short_path))
                runfiles.append(resource.src)
            else:
                fail("Expected blob {} does not match expected filename {}, got {}".format(dest, name, src_path))
        else:
            fail("Expected blob {} not in resources {}".format(dest, dest_to_resource))

    for (label, dest) in attr.expected_generated_blobs.items():
        args.append("--blobs={}={}".format(dest, label.files.to_list()[0].short_path))
        runfiles.append(label.files.to_list()[0])

    for dest in expected_meta_resource_dests:
        if not dest.startswith("meta/"):
            fail("Expected meta file {} not in `meta/`".format(dest))
        if dest in dest_to_resource:
            name = paths.basename(dest)
            resource = dest_to_resource[dest]
            src_path = resource.src.short_path

            if name.endswith(".cvf"):
                # The CVF filename is not preserved, so do not check it.
                continue
            if not src_path.endswith(name):
                fail("Expected meta file {} does not match expected filename {} - found {}".format(
                    dest,
                    name,
                    src_path,
                ))
        else:
            # TODO(https://fxbug.dev/382485650): Remove exception when addressing the TODO above.
            if not attr.expected_generated_blobs:
                fail("Expected meta file {} not in resources {}".format(dest, dest_to_resource))

    return (args, runfiles)

def _fuchsia_package_checker_test_impl(ctx):
    sdk = get_fuchsia_sdk_toolchain(ctx)
    package_info = ctx.attr.package_under_test[FuchsiaPackageInfo]
    meta_far = package_info.meta_far

    (resource_args, resource_runfiles) = _verify_package_resources(package_info, ctx.attr)

    args = [
        "--far={}".format(sdk.far.short_path),
        "--ffx={}".format(sdk.ffx_package.short_path),
        "--meta_far={}".format(meta_far.short_path),
        "--package_name={}".format(ctx.attr.package_name),
    ] + resource_args

    runfiles = [
        meta_far,
        sdk.far,
        sdk.ffx_package,
    ] + resource_runfiles

    # append the components
    args.extend(["--manifests={}".format(m) for m in ctx.attr.manifests])

    # append the cvf files
    args.extend(["--structured_config_files={}".format(f) for f in ctx.attr.structured_config_files])

    # append the bind bytecode
    if ctx.attr.bind_bytecode:
        args.append("--bind_bytecode={}".format(ctx.attr.bind_bytecode))

    # append the subpackages
    args.extend(["--subpackages={}".format(s) for s in ctx.attr.expected_subpackages])

    # append the expected ABI revision
    args.extend(["--abi-revision={}".format(ctx.attr.expected_abi_revision)])

    runfiles = ctx.runfiles(
        files = runfiles,
    ).merge(ctx.attr._package_checker[DefaultInfo].default_runfiles)

    return [create_python3_shell_wrapper_provider(
        ctx,
        ctx.executable._package_checker.short_path,
        args,
        runfiles,
    )]

fuchsia_package_checker_test = rule(
    doc = """Validate the generated package.""",
    test = True,
    implementation = _fuchsia_package_checker_test_impl,
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION,
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
        "structured_config_files": attr.string_list(
            doc = "A list of expected structured config files in meta/foo.cvf form",
            mandatory = False,
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
        "expected_generated_blobs": attr.label_keyed_string_dict(
            doc = """The list of blobs we expect in the package which are generated by a tool.

            The key is the is a source file to compare the blob to, and the value is the install
            location.
            """,
            mandatory = False,
            # label_keyed_string_dict doesn't play nice with allow_single_file
            allow_files = True,
        ),
        "expected_subpackages": attr.string_list(
            doc = "A list of expected subpackage names",
            mandatory = False,
        ),
        "expected_abi_revision": attr.string(
            doc = "ABI revision we should find in the package, as a string-wrapped hexadecimal integer.",
            mandatory = True,
        ),
        "_package_checker": attr.label(
            default = "//tools:package_checker",
            executable = True,
            cfg = "exec",
        ),
    } | PY_TOOLCHAIN_DEPS,
)
