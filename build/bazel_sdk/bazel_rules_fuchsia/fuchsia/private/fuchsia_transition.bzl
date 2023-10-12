# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities for changing the build configuration to fuchsia."""

load(":fuchsia_select.bzl", "if_fuchsia")
load(":utils.bzl", "alias", "forward_providers", "rule_variants")
load("//:api_version.bzl", "DEFAULT_TARGET_API")
load(":fuchsia_api_level.bzl", "FUCHSIA_API_LEVEL_TARGET_NAME")
load("//fuchsia/constraints/platforms:supported_platforms.bzl", "ALL_SUPPORTED_PLATFORMS", "fuchsia_platforms")

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

CPU_MAP = {
    fuchsia_platforms.x64: "x86_64",
    fuchsia_platforms.arm64: "aarch64",
    fuchsia_platforms.riscv64: "riscv64",
}

_REPO_DEFAULT_API_LEVEL_TARGET_NAME = "//fuchsia:repository_default_fuchsia_api_level"
_OVERRIDE_API_LEVEL_TARGET_NAME = "//fuchsia:override_fuchsia_api_level"

def _update_fuchsia_api_level(settings, attr):
    # The logic for determining what API level to use is as follows. The first
    # value that is true will be used
    # 1. Check if a user has set the value by using the override flag
    # 2. Check the value that is set on the fuchsia_package
    # 3. Check the repository_default_fuchsia_api_level flag
    # 4. fail

    override_value = settings[_OVERRIDE_API_LEVEL_TARGET_NAME]

    # Check if we have a user provided value
    if override_value != "":
        # buildifier: disable=print
        print("Using user provided API level {}".format(override_value))
        return override_value

    # TODO(b/303681889): We are currently using the fuchsia_transition in a way
    # that causes analysis to run on more targets than we should. Some of these
    # targets don't have an api level setting so we need to check for that here.
    if hasattr(attr, "fuchsia_api_level"):
        # If not set on the command  line, use the value in the package rule
        api_level = attr.fuchsia_api_level
        if api_level != "":
            return api_level
    else:
        # We need to return an empty value here because there are some rules which
        # use this tranition but do not have the api level as an attribute. If we
        # return anything else here the api level setting will be set to some
        # value and we will not be able to know if this is a user's intention
        # or a misconfigured transition.
        return ""

    repo_default_value = settings[_REPO_DEFAULT_API_LEVEL_TARGET_NAME]
    if repo_default_value != "":
        return repo_default_value

    #TODO(b/303683945): We should fail here once users are setting their API level.
    return str(DEFAULT_TARGET_API)
    # fail("Packages must be built against an API level.")

def _package_supplied_platform(attr):
    # We should be pulling the platform off of the package but we need to clean
    # up the usages of this transition before we can assume the target of the
    # transition is always a package.
    if hasattr(attr, "platform"):
        platform = attr.platform

        # if platform is not set fwe fall back to our old method for finding the
        # platform until we transition all users.
        if platform != None and platform != "":
            if platform in ALL_SUPPORTED_PLATFORMS:
                return platform
            else:
                fail("ERROR: Attempting to build a fuchsia package with an unsupported platform: ", platform)

    return None

def _fuchsia_transition_impl(settings, attr):
    fuchsia_platform = _package_supplied_platform(attr)

    if fuchsia_platform == None:
        input_cpu = settings["//command_line_option:cpu"]
        output_cpu = NATIVE_CPU_ALIASES.get(input_cpu, None)
    else:
        output_cpu = CPU_MAP[fuchsia_platform]

    if not output_cpu:
        fail("Unrecognized cpu %s." % input_cpu)

    # allow for a soft transition
    if fuchsia_platform == None:
        fuchsia_platform = "@fuchsia_sdk//fuchsia/constraints/platforms:" + FUCHSIA_PLATFORMS_MAP[output_cpu]

    copt = settings["//command_line_option:copt"] + (
        [] if "--debug" in settings["//command_line_option:copt"] else ["--debug"]
    )

    # Note: we do not need to validate here since the validation logic will
    # run in the config setting rule
    fuchsia_api_level = _update_fuchsia_api_level(settings, attr)
    if fuchsia_api_level != "":
        # Make sure our c++ rules target the correct level
        copt.append("-ffuchsia-api-level={}".format(fuchsia_api_level))

    return {
        "//command_line_option:cpu": output_cpu,
        "//command_line_option:crosstool_top": "@fuchsia_clang//:toolchain",
        "//command_line_option:host_crosstool_top": "@bazel_tools//tools/cpp:toolchain",
        "//command_line_option:copt": copt,
        "//command_line_option:strip": "never",
        "//command_line_option:platforms": fuchsia_platform,
        FUCHSIA_API_LEVEL_TARGET_NAME: fuchsia_api_level,
    }

fuchsia_transition = transition(
    implementation = _fuchsia_transition_impl,
    inputs = [
        _OVERRIDE_API_LEVEL_TARGET_NAME,
        _REPO_DEFAULT_API_LEVEL_TARGET_NAME,
        "//command_line_option:cpu",
        "//command_line_option:copt",
    ],
    outputs = [
        FUCHSIA_API_LEVEL_TARGET_NAME,
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
    doc = """Transitions build-only, build + run, or build + test targets.""",
    attrs = {
        "actual": attr.label(
            doc = "The target to transition.",
            mandatory = True,
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
