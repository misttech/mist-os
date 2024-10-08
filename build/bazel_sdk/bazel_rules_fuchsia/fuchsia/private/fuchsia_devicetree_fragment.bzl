# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Defines a devicetree source file that can be included by other devicetree files."""

load(":providers.bzl", "FuchsiaDeviceTreeSegmentInfo")
load(":utils.bzl", "preprocesss_file")

def _fuchsia_devicetree_fragment_impl(ctx):
    source = ctx.file.source
    output = source
    order = "postorder"

    includes_sets = []
    headers = []
    for dep in ctx.attr.deps:
        if FuchsiaDeviceTreeSegmentInfo in dep:
            includes_sets.append(dep[FuchsiaDeviceTreeSegmentInfo].includes)
        if CcInfo in dep:
            context = dep[CcInfo].compilation_context
            includes_sets.append(context.system_includes)
            includes_sets.append(context.quote_includes)
            headers.extend(context.direct_public_headers)

    include_depset = depset(
        transitive = includes_sets,
        order = order,
    )

    if source.extension == "S":
        output = preprocesss_file(ctx, source, include_depset, headers)

    return [
        DefaultInfo(
            files = depset([output]),
        ),
        FuchsiaDeviceTreeSegmentInfo(
            includes = depset(
                [output.dirname],
                transitive = [include_depset],
                order = order,
            ),
        ),
    ]

fuchsia_devicetree_fragment = rule(
    doc = """Defines a devicetree source file that can be included by other
    devicetree files.""",
    implementation = _fuchsia_devicetree_fragment_impl,
    attrs = {
        "deps": attr.label_list(
            doc = "Other Devicetree fragment targets referenced by this fragment",
            providers = [[FuchsiaDeviceTreeSegmentInfo], [CcInfo]],
        ),
        "source": attr.label(
            doc = """Device tree source include file (.dtsi/.dtsi.S). Source
            files that include C header files should end with the extension
            `.dtsi.S` and it will be preprocessed by C compiler.""",
            allow_single_file = [".dtsi", ".dtsi.S"],
        ),
        "_cc_toolchain": attr.label(
            default = Label("@bazel_tools//tools/cpp:current_cc_toolchain"),
        ),
    },
)
