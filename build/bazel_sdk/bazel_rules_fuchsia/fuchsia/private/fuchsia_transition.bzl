# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities for changing the build configuration to fuchsia."""

load(":fuchsia_select.bzl", "if_fuchsia")
load(":utils.bzl", "alias", "forward_providers", "rule_variants")

NATIVE_CPU_ALIASES = {
    "darwin": "x86_64",
    "k8": "x86_64",
    "x86_64": "x86_64",
    "armeabi-v7a": "aarch64",
    "aarch64": "aarch64",
    "darwin_arm64": "aarch64",
    "darwin_x86_64": "x86_64",
    "riscv64": "riscv64",
}

FUCHSIA_PLATFORMS_MAP = {
    "x86_64": "fuchsia_x64",
    "aarch64": "fuchsia_arm64",
    "riscv64": "fuchsia_riscv64",
}

def _fuchsia_transition_impl(settings, _):
    input_cpu = settings["//command_line_option:cpu"]
    output_cpu = NATIVE_CPU_ALIASES.get(input_cpu, None)
    if not output_cpu:
        fail("Unrecognized cpu %s." % input_cpu)
    fuchsia_platform = "@fuchsia_sdk//fuchsia/constraints/platforms:" + FUCHSIA_PLATFORMS_MAP[output_cpu]
    copt = settings["//command_line_option:copt"] + (
        [] if "--debug" in settings["//command_line_option:copt"] else ["--debug"]
    )
    return {
        "//command_line_option:cpu": output_cpu,
        "//command_line_option:crosstool_top": "@fuchsia_clang//:toolchain",
        "//command_line_option:host_crosstool_top": "@bazel_tools//tools/cpp:toolchain",
        "//command_line_option:copt": copt,
        "//command_line_option:strip": "never",
        "//command_line_option:platforms": fuchsia_platform,
    }

fuchsia_transition = transition(
    implementation = _fuchsia_transition_impl,
    inputs = [
        "//command_line_option:cpu",
        "//command_line_option:copt",
    ],
    outputs = [
        "//command_line_option:cpu",
        "//command_line_option:crosstool_top",
        "//command_line_option:host_crosstool_top",
        "//command_line_option:copt",
        "//command_line_option:strip",
        "//command_line_option:platforms",
    ],
)

def _forward_default_info(ctx):
    return forward_providers(ctx, ctx.attr.actual)

(
    _with_fuchsia_transition,
    _with_fuchsia_transition_for_run,
    _with_fuchsia_transition_for_test,
) = rule_variants(
    variants = (None, "executable", "test"),
    implementation = _forward_default_info,
    cfg = fuchsia_transition,
    doc = """Transitions build-only, build + run, or build + test targets.""",
    attrs = {
        "actual": attr.label(
            doc = "The target to transition.",
            mandatory = True,
        ),
        "_allowlist_function_transition": attr.label(
            default = "@bazel_tools//tools/allowlists/function_transition_allowlist",
        ),
    },
)

def with_fuchsia_transition(
        *,
        name,
        actual,
        executable = True,
        testonly = False,
        **kwargs):
    """
    Applies fuchsia_transition on a target.

    Args:
        name: The target name.
        actual: The target to apply to.
        executable: Whether `target`[DefaultInfo] has an executable.
        testonly: Whether this is a test target.
        **kwargs: Additional kwargs to forward to the rule.
    """
    if not executable:
        transition = _with_fuchsia_transition
    elif not testonly:
        transition = _with_fuchsia_transition_for_run
    else:
        transition = _with_fuchsia_transition_for_test
    transition(
        name = name + "_with_transition",
        actual = actual,
        testonly = testonly,
        **kwargs
    )
    alias(
        name = name,
        actual = if_fuchsia(
            actual,
            if_not = name + "_with_transition",
        ),
        executable = executable,
        testonly = testonly,
        **kwargs
    )
