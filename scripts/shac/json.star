# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "compiled_tool_path")

_JSON5_EXTS = (
    ".json5",
    ".persist",
    ".triage",
)

def _json5_format(ctx):
    """Runs `formatjson5` on .json5 files.

    Args:
      ctx: A ctx instance.
    """
    exe = compiled_tool_path(ctx, "formatjson5")
    cml_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith((_JSON5_EXTS))
    ]

    for f in cml_files:
        formatted = ctx.os.exec([exe, f]).wait().stdout
        original = str(ctx.io.read_file(f))
        if formatted != original:
            ctx.emit.finding(
                level = "warning",
                message = FORMATTER_MSG,
                filepath = f,
                replacements = [formatted],
            )

def register_json_checks():
    shac.register_check(shac.check(_json5_format, formatter = True))
