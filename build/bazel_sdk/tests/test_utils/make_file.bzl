# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

def _make_file_impl(ctx):
    f = ctx.actions.declare_file(ctx.label.name + "_" + ctx.attr.filename)
    ctx.actions.write(f, ctx.attr.content)
    return DefaultInfo(files = depset([f]))

make_file = rule(
    implementation = _make_file_impl,
    doc = """A simple rule for making a file.

    This could be achieved with a genrule that cats to a file but this provides
    a simpler interface.""",
    attrs = {
        "filename": attr.string(),
        "content": attr.string(),
    },
)
