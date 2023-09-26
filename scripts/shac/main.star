# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# keep-sorted start
load("./cml.star", "register_cml_checks")
load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "compiled_tool_path")
load("./dart.star", "register_dart_checks")
load("./fidl.star", "register_fidl_checks")
load("./go.star", "register_go_checks")
load("./json.star", "register_json_checks")
load("./keep_sorted.star", "keep_sorted")
load("./python.star", "register_python_checks")
load("./rust.star", "register_rust_checks")
load("./starlark.star", "register_starlark_checks")
# keep-sorted end

def _gn_format(ctx):
    """Runs gn format on .gn and .gni files.

    Args:
      ctx: A ctx instance.
    """
    gn_files = [
        f
        for f in ctx.scm.affected_files()
        if f.endswith((".gn", ".gni"))
    ]
    if not gn_files:
        return

    gn = "prebuilt/third_party/gn/%s/gn" % cipd_platform_name(ctx)

    unformatted_files = ctx.os.exec(
        [gn, "format", "--dry-run"] + gn_files,
        ok_retcodes = [0, 2],
    ).wait().stdout.splitlines()

    for f in unformatted_files:
        formatted_contents = ctx.os.exec(
            [gn, "format", "--stdin"],
            stdin = ctx.io.read_file(f),
        ).wait().stdout
        ctx.emit.finding(
            level = "error",
            message = FORMATTER_MSG,
            filepath = f,
            replacements = [formatted_contents],
        )

def _doc_checker(ctx):
    """Runs the doc-checker tool."""

    # If a Markdown change is present (including a deletion of a markdown file),
    # check the entire project.
    if not any([f.endswith(".md") for f in ctx.scm.affected_files()]):
        return

    exe = compiled_tool_path(ctx, "doc-checker")
    res = ctx.os.exec([exe, "--local-links-only"], ok_retcodes = [0, 1]).wait()
    if res.retcode == 0:
        return
    lines = res.stdout.split("\n")
    for i in range(0, len(lines), 4):
        # The doc-checker output contains 4-line plain text entries of the
        # form:
        # """Error
        # /path/to/file:<line_number>
        # The error message
        # """
        if i + 4 > len(lines):
            break
        _, location, msg, _ = lines[i:i + 4]
        abspath = location.split(":", 1)[0]
        ctx.emit.finding(
            level = "error",
            filepath = abspath[len(ctx.scm.root) + 1:],
            message = msg + "\n\n" + "Run `fx doc-checker --local-links-only` to reproduce.",
        )

def _mdlint(ctx):
    """Runs mdlint."""
    rfc_dir = "docs/contribute/governance/rfcs/"
    affected_files = set(ctx.scm.affected_files())
    if not any([f.startswith(rfc_dir) for f in affected_files]):
        return
    mdlint = compiled_tool_path(ctx, "mdlint")
    res = ctx.os.exec(
        [
            mdlint,
            "--json",
            "--root-dir",
            rfc_dir,
            "--enable",
            "all",
            "--filter-filenames",
            rfc_dir,
        ],
        ok_retcodes = [0, 1],
    ).wait()
    for finding in json.decode(res.stderr):
        if finding["path"] not in ctx.scm.affected_files():
            continue
        ctx.emit.finding(
            level = "warning",
            message = finding["message"],
            filepath = finding["path"],
            line = finding["start_line"],
            end_line = finding["end_line"],
            col = finding["start_char"] + 1,
            end_col = finding["end_char"] + 1,
        )

def register_all_checks():
    """Register all checks that should run.

    Checks must be registered in a callback function because they can only be
    registered by the root shac.star file, not at the top level of any `load`ed
    file.
    """
    shac.register_check(shac.check(_gn_format, formatter = True))
    shac.register_check(shac.check(
        _doc_checker,
        # TODO(olivernewman): doc-checker has historically been run from `fx
        # format-code` even though it's not a formatter and doesn't write
        # results back to disk. Determine whether anyone depends on doc-checker
        # running with `fx format-code`, and unset `formatter = True`.
        formatter = True,
    ))
    shac.register_check(shac.check(_mdlint))
    shac.register_check(keep_sorted)
    register_cml_checks()
    register_dart_checks()
    register_fidl_checks()
    register_go_checks()
    register_json_checks()
    register_python_checks()
    register_rust_checks()
    register_starlark_checks()
