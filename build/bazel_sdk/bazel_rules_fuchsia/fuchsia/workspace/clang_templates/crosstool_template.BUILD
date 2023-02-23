# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("@bazel_skylib//lib:selects.bzl", "selects")
load(":cc_toolchain_config.bzl", "cc_toolchain_config", "feature_flag")
load("@rules_fuchsia//fuchsia:defs.bzl", "fuchsia_debug_symbols", "fuchsia_package_resource_group")

licenses(["notice"])

package(default_visibility = ["//visibility:public"])

cc_toolchain_suite(
    name = "toolchain",
    toolchains = {
        "aarch64|llvm": ":cc-compiler-aarch64",
        "aarch64": ":cc-compiler-aarch64",
        "x86_64|llvm": ":cc-compiler-x86_64",
        "x86_64": ":cc-compiler-x86_64",
    },
)

TARGET_CPUS = [
    "aarch64",
    "x86_64",
]

filegroup(
    name = "empty",
)

filegroup(
    name = "cc-compiler-prebuilts",
    srcs = [
        "//:bin/clang",
        "//:bin/clang++",
        "//:bin/clang-cpp",
        "//:bin/llvm-strip",
    ],
)

filegroup(
    name = "cc-linker-prebuilts",
    srcs = [
        "//:bin/clang",
        "//:bin/ld.lld",
        "//:bin/ld64.lld",
        "//:bin/lld",
        "//:bin/lld-link",
    ],
)

filegroup(
    name = "libunwind-headers",
    srcs = [
        "include/libunwind.h",
        "include/libunwind.modulemap",
        "include/mach-o/compact_unwind_encoding.h",
        "include/mach-o/compact_unwind_encoding.modulemap",
        "include/unwind.h",
        "include/unwind_arm_ehabi.h",
        "include/unwind_itanium.h",
    ],
)

[
    filegroup(
        name = "libcxx-headers-" + cpu,
        srcs = glob([
            "include/c++/v1/**",
        ]) + glob([
            # TODO(fxbug.dev/91180): Try not to hard code this path.
            "lib/clang/%{CLANG_VERSION}/include/**",
        ]) + glob([
            "include/%s-unknown-fuchsia/c++/v1/*" % cpu,
        ]),
    )
    for cpu in TARGET_CPUS
]

[
    filegroup(
        name = "libcxx-libraries-" + cpu,
        srcs = glob([
            "lib/%s-unknown-fuchsia/libc++.*" % cpu,
            "lib/%s-unknown-fuchsia/libc++abi.*" % cpu,
            "lib/%s-unknown-fuchsia/libunwind.*" % cpu,
            "lib/%s-unknown-fuchsia/asan/libc++.*" % cpu,
            "lib/%s-unknown-fuchsia/asan/libc++abi.*" % cpu,
            "lib/%s-unknown-fuchsia/asan/libunwind.*" % cpu,
        ]),
    )
    for cpu in TARGET_CPUS
]

filegroup(
    name = "ar",
    srcs = ["//:bin/llvm-ar"],
)

[
    filegroup(
        name = "fuchsia-sysroot-headers-" + cpu,
        srcs = glob(["fuchsia_sysroot_" + cpu + "/include/**"]),
    )
    for cpu in TARGET_CPUS
]

[
    filegroup(
        name = "fuchsia-sysroot-libraries-" + cpu,
        srcs = glob(["fuchsia_sysroot_" + cpu + "/lib/**"]),
    )
    for cpu in TARGET_CPUS
]

[
    filegroup(
        name = "compile-" + cpu,
        srcs = [
            ":cc-compiler-prebuilts",
            ":libunwind-headers",
            ":libcxx-headers-" + cpu,
            ":fuchsia-sysroot-headers-" + cpu,
        ],
    )
    for cpu in TARGET_CPUS
]

filegroup(
    name = "objcopy",
    srcs = [
        "//:bin/llvm-objcopy",
    ],
)

filegroup(
    name = "strip",
    srcs = [
        "//:bin/llvm-strip",
    ],
)

filegroup(
    name = "nm",
    srcs = [
        "//:bin/llvm-nm",
    ],
)

[
    filegroup(
        name = "every-file-" + cpu,
        srcs = [
            ":compile-" + cpu,
            ":runtime-" + cpu,
            ":link-" + cpu,
            ":ar",
            ":nm",
            ":objcopy",
        ],
    )
    for cpu in TARGET_CPUS
]

[
    filegroup(
        name = "link-" + cpu,
        srcs = [
            ":cc-linker-prebuilts",
            ":fuchsia-sysroot-libraries-" + cpu,
            ":libcxx-libraries-" + cpu,
            ":runtime-" + cpu,
        ],
    )
    for cpu in TARGET_CPUS
]

[
    filegroup(
        name = "runtime-" + cpu,
        srcs = [
            # TODO(fxbug.dev/91180): Don't hard code this path.
            "//:lib/clang/%{CLANG_VERSION}/lib/%s-unknown-fuchsia/libclang_rt.builtins.a" % cpu,
        ],
    )
    for cpu in TARGET_CPUS
]

[
    cc_toolchain_config(
        name = "crosstool-1.x.x-llvm-fuchsia-config-" + cpu,
        cpu = cpu,
    )
    for cpu in TARGET_CPUS
]

[
    cc_toolchain(
        name = "cc-compiler-" + cpu,
        all_files = ":every-file-" + cpu,
        ar_files = ":ar",
        compiler_files = ":compile-" + cpu,
        dwp_files = ":empty",
        dynamic_runtime_lib = ":runtime-" + cpu,
        linker_files = ":link-" + cpu,
        objcopy_files = ":objcopy",
        static_runtime_lib = ":runtime-" + cpu,
        strip_files = ":strip",
        supports_param_files = 1,
        toolchain_config = "crosstool-1.x.x-llvm-fuchsia-config-" + cpu,
        toolchain_identifier = "crosstool-1.x.x-llvm-fuchsia-" + cpu,
    )
    for cpu in TARGET_CPUS
]

[
    toolchain(
        name = "cc-" + cpu,
        target_compatible_with = [
            "@platforms//cpu:" + cpu,
            "@platforms//os:fuchsia",
        ],
        toolchain = ":cc-compiler-" + cpu,
        toolchain_type = "@bazel_tools//tools/cpp:toolchain_type",
    )
    for cpu in TARGET_CPUS
]

cc_library(
    name = "sources",
    srcs = glob(["src/**"]),
    visibility = ["//visibility:public"],
)

fuchsia_debug_symbols(
    name = "debug_symbols",
    build_dir = "//:BUILD.bazel",
    build_id_dirs = ["//:lib/debug/.build-id"],
)

fuchsia_package_resource_group(
    name = "dist",
    srcs = select({
        ":arm_novariant": [
            "//:lib/aarch64-unknown-fuchsia/libc++.so.2",
            "//:lib/aarch64-unknown-fuchsia/libc++abi.so.1",
            "//:lib/aarch64-unknown-fuchsia/libunwind.so.1",
        ],
        ":arm_asan_variant": [
            "//:lib/aarch64-unknown-fuchsia/asan/libc++.so.2",
            "//:lib/aarch64-unknown-fuchsia/asan/libc++abi.so.1",
            "//:lib/aarch64-unknown-fuchsia/asan/libunwind.so.1",
        ],
        ":x86_novariant": [
            "//:lib/x86_64-unknown-fuchsia/libc++.so.2",
            "//:lib/x86_64-unknown-fuchsia/libc++abi.so.1",
            "//:lib/x86_64-unknown-fuchsia/libunwind.so.1",
        ],
        ":x86_asan_variant": [
            "//:lib/x86_64-unknown-fuchsia/asan/libc++.so.2",
            "//:lib/x86_64-unknown-fuchsia/asan/libc++abi.so.1",
            "//:lib/x86_64-unknown-fuchsia/asan/libunwind.so.1",
        ],
    }),
    dest = "lib" + select({
        ":asan_variant": "/asan",
        "//conditions:default": "",
    }),
    strip_prefix = select({
        ":arm_novariant": "../fuchsia_clang/lib/aarch64-unknown-fuchsia",
        ":arm_asan_variant": "../fuchsia_clang/lib/aarch64-unknown-fuchsia/asan",
        ":x86_novariant": "../fuchsia_clang/lib/x86_64-unknown-fuchsia",
        ":x86_asan_variant": "../fuchsia_clang/lib/x86_64-unknown-fuchsia/asan",
    }),
    visibility = ["//visibility:public"],
)

fuchsia_package_resource_group(
    name = "runtime",
    srcs = select({
        ":arm_asan_variant": [
            "//:lib/clang/%{CLANG_VERSION}/lib/aarch64-unknown-fuchsia/libclang_rt.asan.so",
        ],
        ":x86_asan_variant": [
            "//:lib/clang/%{CLANG_VERSION}/lib/x86_64-unknown-fuchsia/libclang_rt.asan.so",
        ],
        "//conditions:default": [],
    }),
    dest = "lib" + select({
        ":asan_variant": "/asan",
        "//conditions:default": "",
    }),
    strip_prefix = select({
        ":arm_build": "../fuchsia_clang/lib/clang/%{CLANG_VERSION}/lib/aarch64-unknown-fuchsia",
        ":x86_build": "../fuchsia_clang/lib/clang/%{CLANG_VERSION}/lib/x86_64-unknown-fuchsia",
    }),
    visibility = ["//visibility:public"],
)

config_setting(
    name = "aarch64_build",
    values = {"cpu": "aarch64"},
)

config_setting(
    name = "aarch64_platform_build",
    constraint_values = [
        "@platforms//cpu:aarch64",
    ],
)

config_setting(
    name = "armeabi-v7a_build",
    values = {"cpu": "armeabi-v7a"},
)

selects.config_setting_group(
    name = "arm_build",
    match_any = [
        ":aarch64_build",
        ":aarch64_platform_build",
        ":armeabi-v7a_build",
    ],
)

config_setting(
    name = "k8_build",
    values = {"cpu": "k8"},
)

config_setting(
    name = "x86_64_build",
    values = {"cpu": "x86_64"},
)

config_setting(
    name = "x86_64_platform_build",
    constraint_values = [
        "@platforms//cpu:x86_64",
    ],
)

selects.config_setting_group(
    name = "x86_build",
    match_any = [
        ":k8_build",
        ":x86_64_build",
        ":x86_64_platform_build",
    ],
)

feature_flag(
    name = "asan_flag",
    feature_name = "asan",
    visibility = ["//visibility:private"],
)

config_setting(
    name = "novariant",
    flag_values = {
        ":asan_flag": "False",
    },
    visibility = ["//visibility:public"],
)

config_setting(
    name = "asan_variant",
    flag_values = {
        ":asan_flag": "True",
    },
    visibility = ["//visibility:public"],
)

selects.config_setting_group(
    name = "arm_novariant",
    match_all = [
        ":arm_build",
        ":novariant",
    ],
)

selects.config_setting_group(
    name = "arm_asan_variant",
    match_all = [
        ":arm_build",
        ":asan_variant",
    ],
)

selects.config_setting_group(
    name = "x86_novariant",
    match_all = [
        ":x86_build",
        ":novariant",
    ],
)

selects.config_setting_group(
    name = "x86_asan_variant",
    match_all = [
        ":x86_build",
        ":asan_variant",
    ],
)
