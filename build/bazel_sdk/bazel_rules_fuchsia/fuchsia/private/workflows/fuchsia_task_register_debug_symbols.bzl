# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Registers debug symbols with ffx as a task workflow."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:fuchsia_debug_symbols.bzl", "collect_debug_symbols")
load(":fuchsia_shell_task.bzl", "shell_task_rule")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")

def _fuchsia_task_register_debug_symbols_impl(ctx, make_shell_task):
    sdk = get_fuchsia_sdk_toolchain(ctx)
    build_id_dirs, build_dirs = zip(*[
        (build_id_dir, build_dir)
        for build_dir, build_id_dirs in collect_debug_symbols(ctx.attr.deps).build_id_dirs.items()
        for build_id_dir in build_id_dirs.to_list()
    ]) or ([], [])

    return make_shell_task(
        command = [
            ctx.attr._tool,
            "--ffx",
            sdk.ffx,
            "--build-id-dirs",
        ] + list(build_id_dirs) + [
            "--build-dirs",
        ] + list(build_dirs),
    )

(
    _fuchsia_task_register_debug_symbols,
    _fuchsia_task_register_debug_symbols_for_test,
    fuchsia_task_register_debug_symbols,
) = shell_task_rule(
    doc = """Registers debug symbols with ffx.""",
    toolchains = FUCHSIA_TOOLCHAIN_DEFINITION,
    implementation = _fuchsia_task_register_debug_symbols_impl,
    attrs = {
        "_tool": attr.label(
            doc = "The tool needed to register debug symbols.",
            default = "//fuchsia/tools:register_debug_symbols",
        ),
        "deps": attr.label_list(
            doc = """Collects FuchsiaDebugSymbolInfo across multiple dependencies.
            If a dependency does not provide the FuchsiaDebugSymbolInfo it will be ignored.
            """,
            mandatory = True,
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)
