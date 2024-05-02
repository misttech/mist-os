# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "get_fuchsia_dir", "os_exec")
load("//third_party/shac-project/checks-starlark/src/register.star", "register_buildifier_format", "register_buildifier_lint")

def _buildifier_tool(ctx):
    return "%s/prebuilt/third_party/buildifier/%s/buildifier" % (
        get_fuchsia_dir(ctx),
        cipd_platform_name(ctx),
    )

def _file_filter(file):
    return not file.startswith("third_party/")

def _fuchsia_shac_style_guide(ctx):
    """Enforces shac conventions that are specific to the fuchsia project."""
    starlark_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith(".star")
    ]
    procs = []
    for f in starlark_files:
        procs.append(
            (f, os_exec(ctx, [
                "%s/prebuilt/third_party/python3/%s/bin/python3" % (
                    get_fuchsia_dir(ctx),
                    cipd_platform_name(ctx),
                ),
                "scripts/shac/fuchsia_shac_style_guide.py",
                f,
            ])),
        )

    for f, proc in procs:
        res = proc.wait()
        for finding in json.decode(res.stdout):
            ctx.emit.finding(
                level = "error",
                filepath = f,
                message = finding["message"],
                line = finding["line"],
                col = finding["col"],
                end_line = finding["end_line"],
                end_col = finding["end_col"],
            )

def register_starlark_checks():
    shac.register_check(shac.check(_fuchsia_shac_style_guide))

    register_buildifier_lint(
        tool_ctx = _buildifier_tool,
        file_filter = _file_filter,
        emit_message = "Please address linter issues.",
        emit_level = "warning",
    )
    register_buildifier_format(
        tool_ctx = _buildifier_tool,
        file_filter = _file_filter,
        emit_message = FORMATTER_MSG,
    )
