# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "compiled_tool_path", "os_exec")

def _filter_fidl_files(files):
    return [
        f
        for f in files
        if f.endswith(".fidl") and
           # FIDL test files are often purposefully invalid.
           not f.endswith(".test.fidl")
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

def _fidl_lint(ctx):
    """Runs fidl-lint.

    Args:
        ctx: A ctx instance.
    """
    fidl_files = _filter_fidl_files(ctx.scm.affected_files())
    if not fidl_files:
        return

    results = json.decode(os_exec(
        ctx,
        [
            compiled_tool_path(ctx, "fidl-lint"),
            "--format=json",
        ] + fidl_files,
        ok_retcodes = [0, 1],
    ).wait().stdout)

    for result in results:
        replacements = []
        for s in result["suggestions"]:
            for repl in s["replacements"]:
                # Only consider replacements that cover the same span as the
                # finding.
                if all([
                    repl[field] == result[field]
                    for field in [
                        "path",
                        "start_line",
                        "start_char",
                        "end_line",
                        "end_char",
                    ]
                ]):
                    replacements.append(repl["replacement"])
        ctx.emit.finding(
            level = "warning",
            message = result["message"],
            filepath = result["path"],
            line = result["start_line"],
            col = result["start_char"] + 1,
            end_line = result["end_line"],
            end_col = (
                result["end_char"] + 1 if result["start_char"] != result["end_char"] else 0
            ),
            replacements = replacements,
        )

def register_fidl_checks():
    shac.register_check(shac.check(_gidl_format, formatter = True))
    shac.register_check(shac.check(_fidl_format, formatter = True))
    shac.register_check(shac.check(_fidl_lint))
