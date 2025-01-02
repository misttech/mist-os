# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# keep-sorted start
load("./cml.star", "register_cml_checks")
load("./common.star", "FORMATTER_MSG", "cipd_platform_name", "get_fuchsia_dir", "os_exec")
load("./dart.star", "register_dart_checks")
load("./docs.star", "register_doc_checks")
load("./fidl.star", "register_fidl_checks")
load("./go.star", "register_go_checks")
load("./json.star", "register_json_checks")
load("./keep_sorted.star", "keep_sorted")
load("./owners.star", "register_owners_checks")
load("./python.star", "register_python_checks")
load("./rust.star", "register_rust_checks")
load("./starlark.star", "register_starlark_checks")
load("./underscore_vs_dash.star", "register_underscore_vs_dash_checks")
# keep-sorted end

def bug_urls(ctx):
    """Checks that fuchsia bug URLs are correctly formatted.

    Bug URLs should use the form "https://fxbug.dev/42074375"; the form
    "http://fxb/123456" isn't usable by non-Google employees, and
    "fxbug.dev/42074375" doesn't automatically linkify in most editors.

    Args:
        ctx: A ctx instance.
    """
    correct_format = "https://fxbug.dev/"
    for f, meta in ctx.scm.affected_files().items():
        for num, line in meta.new_lines():
            for match in ctx.re.allmatches(
                r"(https?://)?fxb(ug\.dev)?/(\d+)",
                line,
            ):
                if match.groups[0].startswith(correct_format):
                    continue
                bug_number = match.groups[-1]
                repl = correct_format + bug_number
                ctx.emit.finding(
                    level = "warning",
                    message = "Bug links should use the form %s." % repl,
                    filepath = f,
                    line = num,
                    col = match.offset + 1,
                    end_col = match.offset + 1 + len(match.groups[0]),
                    replacements = [repl],
                )

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

    gn = "%s/prebuilt/third_party/gn/%s/gn" % (get_fuchsia_dir(ctx), cipd_platform_name(ctx))

    result = os_exec(
        ctx,
        [gn, "format", "--dry-run"] + gn_files,
        ok_retcodes = [0, 1, 2],
    ).wait()

    if result.retcode in [0, 2]:
        unformatted_files = result.stdout.splitlines()
        for f in unformatted_files:
            formatted_contents = os_exec(
                ctx,
                [gn, "format", "--stdin"],
                stdin = ctx.io.read_file(f),
            ).wait().stdout
            ctx.emit.finding(
                level = "error",
                message = FORMATTER_MSG,
                filepath = f,
                replacements = [formatted_contents],
            )

        # If gn format --dry-run command fails, we can't filter a list of unformatted files, so we iterate over all the files
    elif result.retcode == 1:
        for f in gn_files:
            res = os_exec(
                ctx,
                [gn, "format", "--stdin"],
                stdin = ctx.io.read_file(f),
                ok_retcodes = [0, 1, 2],
            ).wait()
            if res.retcode == 1:
                finding = res.stdout
                fail("{}: {}".format(f, finding))

def register_all_checks():
    """Register all checks that should run.

    Checks must be registered in a callback function because they can only be
    registered by the root shac.star file, not at the top level of any `load`ed
    file.
    """
    shac.register_check(shac.check(_gn_format, formatter = True))
    shac.register_check(keep_sorted)
    shac.register_check(bug_urls)

    # keeps-sorted start
    register_cml_checks()
    register_dart_checks()
    register_doc_checks()
    register_fidl_checks()
    register_go_checks()
    register_json_checks()
    register_owners_checks()
    register_python_checks()
    register_rust_checks()
    register_starlark_checks()
    register_underscore_vs_dash_checks()
    # keeps-sorted end
