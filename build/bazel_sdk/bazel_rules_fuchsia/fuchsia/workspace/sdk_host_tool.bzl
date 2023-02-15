# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# buildifier: disable=module-docstring
# buildifier: disable=function-docstring
def _sdk_host_tool_impl(ctx):
    sdk = ctx.toolchains["@rules_fuchsia//fuchsia:toolchain"]
    file = getattr(sdk, ctx.label.name)
    exe = ctx.actions.declare_file(ctx.label.name + "_wrap.sh")
    ctx.actions.write(exe, """
    #!/bin/bash
    $0.runfiles/{}/{} "$@"
    """.format(ctx.workspace_name, file.short_path), is_executable = True)

    return [DefaultInfo(
        executable = exe,
        runfiles = ctx.runfiles([file]),
    )]

sdk_host_tool = rule(
    implementation = _sdk_host_tool_impl,
    doc = """
    A rule which can wrap tools found in the fuchsia sdk toolchain.

    The rule will look for the name of the tool to be invoked based
    on the name of the target. These targets can then be executed
    directly or the user can use the run_sdk_tool shell script.

    The following rule will wrap the ffx binary.
    ```
    sdk_host_tool(name = "ffx")
    ```
    """,
    toolchains = ["@rules_fuchsia//fuchsia:toolchain"],
    executable = True,
)
