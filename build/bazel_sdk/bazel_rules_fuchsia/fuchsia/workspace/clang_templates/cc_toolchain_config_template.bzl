# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Clang toolchain configuration.
"""

load("@bazel_tools//tools/build_defs/cc:action_names.bzl", "ACTION_NAMES")
load(
    "@bazel_tools//tools/cpp:cc_toolchain_config_lib.bzl",
    "tool_path",
)
load("//common:toolchains/clang/cc_features.bzl", "features")
load("//common:toolchains/clang/toolchain_utils.bzl", "compute_clang_features")
load("//common/platforms:utils.bzl", "to_fuchsia_cpu_name")

def bazel_cpu_to_fuchsia_cpu(cpu):
    """Converts the Bazel cpu type to the Fuchsia cpu"""
    _MAP = {
        "aarch64": "arm64",
        "x86_64": "x64",
    }
    return _MAP.get(cpu, cpu)

_SYSROOT_PATHS = {
    "aarch64": "%{SYSROOT_PATH_AARCH64}",
    "riscv64": "%{SYSROOT_PATH_RISCV64}",
    "x86_64": "%{SYSROOT_PATH_X86_64}",
}

def _cc_toolchain_config_impl(ctx):
    target_system_name = ctx.attr.cpu + "-unknown-fuchsia"
    tool_paths = [
        tool_path(name = "ar", path = "bin/llvm-ar"),
        tool_path(name = "cpp", path = "bin/clang++"),
        tool_path(name = "gcc", path = "bin/clang"),
        tool_path(name = "lld", path = "bin/lld"),
        tool_path(name = "objdump", path = "bin/llvm-objdump"),
        tool_path(name = ACTION_NAMES.strip, path = "bin/llvm-strip"),
        tool_path(name = "nm", path = "bin/llvm-nm"),
        tool_path(name = "objcopy", path = "bin/llvm-objcopy"),
        tool_path(name = "dwp", path = "/not_available/dwp"),
        tool_path(name = "compat-ld", path = "/not_available/compat-ld"),
        tool_path(name = "gcov", path = "/not_available/gcov"),
        tool_path(name = "gcov-tool", path = "/not_available/gcov-tool"),
        tool_path(name = "ld", path = "bin/ld.lld"),
    ]

    cc_features = [
        features.default_compile_flags,
        features.target_system_name(target_system_name),
        features.supports_pic,
        features.no_runtime_library_search_directories,
    ]
    cc_features += compute_clang_features(
        "{HOST_OS}",
        "{HOST_CPU}",
        "fuchsia",
        to_fuchsia_cpu_name(ctx.attr.cpu),
    )

    return cc_common.create_cc_toolchain_config_info(
        ctx = ctx,
        toolchain_identifier = "crosstool-1.x.x-llvm-fuchsia-" + ctx.attr.cpu,
        host_system_name = "x86_64-unknown-linux-gnu",
        target_system_name = target_system_name,
        target_cpu = ctx.attr.cpu,
        target_libc = "fuchsia",
        compiler = "llvm",
        abi_version = "local",
        abi_libc_version = "local",
        tool_paths = tool_paths,
        # Implicit dependencies for Fuchsia system functionality
        cxx_builtin_include_directories = [
            "%sysroot%/include",  # Platform parts of libc.
            "$sysroot$/../include/%s-unknown-fuchsia/c++/v1" % ctx.attr.cpu,  # Platform libc++.
            "$sysroot$/../include/c++/v1",  # Platform libc++.
            "$sysroot$/../lib/clang/%{CLANG_VERSION}/include",  # Platform libc++.
            "$sysroot$/../lib/clang/%{CLANG_VERSION}/share",  # Platform libc++.
        ],
        builtin_sysroot = _SYSROOT_PATHS[ctx.attr.cpu],
        cc_target_os = "fuchsia",
        features = cc_features,
    )

cc_toolchain_config = rule(
    implementation = _cc_toolchain_config_impl,
    attrs = {
        "cpu": attr.string(mandatory = True, values = ["aarch64", "riscv64", "x86_64"]),
    },
    provides = [CcToolchainConfigInfo],
)
