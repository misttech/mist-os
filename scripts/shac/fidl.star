# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "compiled_tool_path")

def _fidl_format(ctx):
    """Runs fidl-format.

    Args:
      ctx: A ctx instance.
    """
    exe = compiled_tool_path(ctx, "fidl-format")
    fidl_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith(".fidl") and
           # FIDL test files are often purposefully invalid.
           not f.endswith(".test.fidl")
    ]

    for f in fidl_files:
        formatted = ctx.os.exec([exe, f]).wait().stdout
        original = str(ctx.io.read_file(f))
        if formatted != original:
            ctx.emit.finding(
                level = "error",
                message = FORMATTER_MSG,
                filepath = f,
                replacements = [formatted],
            )

def register_fidl_checks():
    shac.register_check(shac.check(_fidl_format, formatter = True))
