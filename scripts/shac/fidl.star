# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "compiled_tool_path", "os_exec")

def _filter_fidl_files(files):
    return [
        f
        for f in files
        if f.endswith(".fidl") and
           # These files intentionally test parsing failures.
           not f.endswith(".noformat.test.fidl")
    ]

def _fidl_format(ctx):
    """Runs fidl-format.

    Args:
      ctx: A ctx instance.
    """
    exe = compiled_tool_path(ctx, "fidl-format")

    procs = []
    for f in _filter_fidl_files(ctx.scm.affected_files()):
        procs.append((f, os_exec(ctx, [exe, f])))
    for f, proc in procs:
        formatted = proc.wait().stdout
        original = str(ctx.io.read_file(f))
        if formatted != original:
            ctx.emit.finding(
                level = "error",
                message = FORMATTER_MSG,
                filepath = f,
                replacements = [formatted],
            )

def _gidl_format(ctx):
    """Runs gidl-format.

    Args:
      ctx: A ctx instance.
    """
    exe = compiled_tool_path(ctx, "gidl-format")

    procs = [
        (f, os_exec(ctx, [exe, f]))
        for f in ctx.scm.affected_files()
        if f.endswith(".gidl")
    ]
    for f, proc in procs:
        formatted = proc.wait().stdout
        original = str(ctx.io.read_file(f))
        if formatted != original:
            ctx.emit.finding(
                level = "error",
                message = FORMATTER_MSG,
                filepath = f,
                replacements = [formatted],
            )

def register_fidl_checks():
    shac.register_check(shac.check(_gidl_format, formatter = True))
    shac.register_check(shac.check(_fidl_format, formatter = True))
