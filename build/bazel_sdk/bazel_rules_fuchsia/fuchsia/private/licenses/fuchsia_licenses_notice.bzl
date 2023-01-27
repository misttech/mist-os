# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for generating OSS licenses license notice text file."""

def _fuchsia_licenses_notice(ctx):
    notice_file = ctx.actions.declare_file("%s" % ctx.attr.name)

    ctx.actions.run(
        progress_message = "Generating licenses notice file %s" % notice_file.path,
        inputs = [ctx.file.spdx_input],
        outputs = [notice_file],
        executable = ctx.executable._generate_licenses_notice_tool,
        arguments = [
            "--spdx_input=%s" % ctx.file.spdx_input.path,
            "--output_file=%s" % notice_file.path,
        ],
    )

    return [DefaultInfo(files = depset([notice_file]))]

fuchsia_licenses_notice = rule(
    doc = """
Produces a licenses notice text file from the given SPDX file.
""",
    implementation = _fuchsia_licenses_notice,
    attrs = {
        "spdx_input": attr.label(
            doc = "The target to aggregate the licenses from.",
            allow_single_file = True,
            mandatory = True,
        ),
        "_generate_licenses_notice_tool": attr.label(
            executable = True,
            cfg = "exec",
            default = "//fuchsia/tools/licenses:generate_licenses_notice",
        ),
    },
)
