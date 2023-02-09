# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Custom rule use by test."""

def _list_directory_impl(ctx):
    output_file = ctx.actions.declare_file(ctx.attr.output)

    input_files = ctx.attr.src_dir[DefaultInfo].files

    ctx.actions.run_shell(
        outputs = [output_file],
        inputs = input_files,
        arguments = [f.path for f in input_files.to_list()],
        command = "ls -1 \"$@\" > " + str(output_file.path),
    )

    return [DefaultInfo(files = depset([output_file]))]

list_directory = rule(
    implementation = _list_directory_impl,
    attrs = {
        "src_dir": attr.label(
            doc = "Source directory filegroup() label",
            mandatory = True,
        ),
        "output": attr.string(
            doc = "Output file name",
            mandatory = True,
        ),
    },
)
