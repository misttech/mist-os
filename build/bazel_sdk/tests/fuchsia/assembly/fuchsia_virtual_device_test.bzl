# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
load("@fuchsia_sdk//fuchsia/private/assembly:providers.bzl", "FuchsiaVirtualDeviceInfo")
load(":test_utils.bzl", "CREATE_VALIDATION_SCRIPT_ATTRS", "create_validation_script")

def _fuchsia_virtual_device_test_impl(ctx):
    virtual_device_file = ctx.attr.virtual_device[FuchsiaVirtualDeviceInfo].config
    golden_file = ctx.file.golden_file
    script, runfiles = create_validation_script(ctx, virtual_device_file, golden_file)
    return [
        DefaultInfo(
            executable = script,
            runfiles = runfiles,
            files = depset(
                direct = ctx.files.virtual_device,
            ),
        ),
    ]

fuchsia_virtual_device_test = rule(
    doc = """Validate the generated virtual device config file.""",
    test = True,
    implementation = _fuchsia_virtual_device_test_impl,
    attrs = {
        "virtual_device": attr.label(
            doc = "Built virtual device config.",
            providers = [FuchsiaVirtualDeviceInfo],
            mandatory = True,
        ),
        "golden_file": attr.label(
            doc = "Golden file to match against",
            allow_single_file = True,
            mandatory = True,
        ),
    } | CREATE_VALIDATION_SCRIPT_ATTRS,
)
