# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for modifying a segment in a license file before classification."""

def _fuchsia_licenses_preprocess_impl(ctx):
    spdx_output = ctx.actions.declare_file(ctx.attr.output)
    args = [
        "--input=%s" % ctx.file.input.path,
        "--output=%s" % spdx_output.path,
        "--license_id=%s" % ctx.attr.license_id,
        "--cut_after=%s" % ctx.attr.cut_after,
    ]

    if ctx.attr.cut_entirely:
        args += ["--cut_entirely"]

    if ctx.attr.spdx_id:
        args += ["--spdx_id=%s" % ctx.attr.spdx_id]

    ctx.actions.run(
        progress_message = "Preprocessing license file %s" %
                           (ctx.file.input.path),
        inputs = [ctx.file.input],
        outputs = [spdx_output],
        executable = ctx.executable._remove_license_segment_tool,
        arguments = args,
    )

    return [DefaultInfo(files = depset([spdx_output]))]

fuchsia_licenses_preprocess = rule(
    doc = """
Preprocess a license SPDX file.
""",
    implementation = _fuchsia_licenses_preprocess_impl,
    # buildifier: disable=attr-licenses
    attrs = {
        "input": attr.label(
            doc = "The input licenses.spdx.json file",
            allow_single_file = True,
            mandatory = True,
        ),
        "output": attr.string(
            doc = "The output licenses.spdx.json file",
            mandatory = True,
        ),
        "license_id": attr.string(
            doc = "The name of the license ID in the SPDX file.",
            mandatory = True,
        ),
        "spdx_id": attr.string(
            doc = "The name of the SPDX package ID in the SPDX file.",
        ),
        "cut_after": attr.string(
            doc = "The string pattern where the license text should be cut.",
        ),
        "cut_entirely": attr.bool(
            doc = "Flag instructing whether the entire license with ID 'license_id' should be removed.",
            default = False,
        ),
        "exit_on_failure": attr.bool(
            doc = """Whether or not to fail the build if the given
            license_id or pattern is not found in the given SPDX file.""",
            default = False,
        ),
        "_remove_license_segment_tool": attr.label(
            executable = True,
            cfg = "exec",
            default = "//fuchsia/tools/licenses:remove_license_segment",
        ),
    },
)
