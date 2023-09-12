# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Tests that invoke a python binary to run their test must depend on these deps
# to ensure that the interpreter and all dependent files are available during
# test execution
# my_rule = rule(
#     ...,
#     attr = {
#         "_my_py_bin": attr.label(
#             default = "//tools:foo",
#             executable = True,
#             cfg = "exec"
#         )
#     } | PY_TOOLCHAIN_DEPS
# )
PY_TOOLCHAIN_DEPS = {
    "_py_toolchain": attr.label(
        default = "@rules_python//python:current_py_toolchain",
        cfg = "exec",
    ),
}

def populate_py_test_sh_script(ctx, script, tool, args = []):
    """Populate a file with script content suitable for testing

    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    populate_py_test_sh_script(ctx, script, ctx.executable._my_py_bin, args)
"""
    script_content = """
#!/bin/sh
python3 {tool} {args}""".format(
        tool = tool.short_path,
        args = " \\\n".join(args),
    )
    ctx.actions.write(script, script_content, is_executable = True)
