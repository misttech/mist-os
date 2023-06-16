# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Clang toolchain configuration.
"""

load(
    "@bazel_tools//tools/cpp:cc_toolchain_config_lib.bzl",
    "feature",
    "flag_group",
    "flag_set",
    "tool_path",
    "with_feature_set",
)
load(
    "@bazel_tools//tools/cpp:toolchain_utils.bzl",
    "find_cpp_toolchain",
    "use_cpp_toolchain",
)
load("@bazel_tools//tools/build_defs/cc:action_names.bzl", "ACTION_NAMES")
load("@%{SDK_ROOT}//:api_version.bzl", "DEFAULT_CLANG_TARGET_API")

def _sanitizer_feature(name, fsanitize):
    return feature(
        name = name,
        flag_sets = [
            flag_set(
                actions = [
                    ACTION_NAMES.c_compile,
                    ACTION_NAMES.cpp_compile,
                    ACTION_NAMES.cpp_module_compile,
                    ACTION_NAMES.cpp_link_executable,
                    ACTION_NAMES.cpp_link_dynamic_library,
                    ACTION_NAMES.cpp_link_nodeps_dynamic_library,
                ],
                flag_groups = [flag_group(flags = ["-fsanitize=" + fsanitize])],
            ),
        ],
        implies = ["sanitizer"],
    )

def _cc_toolchain_config_impl(ctx):
    target_system_name = ctx.attr.cpu + "-fuchsia"
    tool_paths = [
        tool_path(
            name = "ar",
            path = "bin/llvm-ar",
        ),
        tool_path(
            name = "cpp",
            path = "bin/clang++",
        ),
        tool_path(
            name = "gcc",
            path = "bin/clang",
        ),
        tool_path(
            name = "lld",
            path = "bin/lld",
        ),
        tool_path(
            name = "objdump",
            path = "bin/llvm-objdump",
        ),
        tool_path(
            name = ACTION_NAMES.strip,
            path = "bin/llvm-strip",
        ),
        tool_path(
            name = "nm",
            path = "bin/llvm-nm",
        ),
        tool_path(
            name = "objcopy",
            path = "bin/llvm-objcopy",
        ),
        tool_path(
            name = "dwp",
            path = "/not_available/dwp",
        ),
        tool_path(
            name = "compat-ld",
            path = "/not_available/compat-ld",
        ),
        tool_path(
            name = "gcov",
            path = "/not_available/gcov",
        ),
        tool_path(
            name = "gcov-tool",
            path = "/not_available/gcov-tool",
        ),
        tool_path(
            name = "ld",
            path = "bin/ld.lld",
        ),
    ]
    features = [
        # Redefine the dependency_file feature to use -MMD instead of -MD
        # to make remote builds work properly.
        feature(
            name = "dependency_file",
            enabled = True,
            flag_sets = [
                flag_set(
                    actions = [
                        ACTION_NAMES.assemble,
                        ACTION_NAMES.preprocess_assemble,
                        ACTION_NAMES.c_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_module_compile,
                        ACTION_NAMES.objc_compile,
                        ACTION_NAMES.objcpp_compile,
                        ACTION_NAMES.cpp_header_parsing,
                        ACTION_NAMES.clif_match,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = ["-MMD", "-MF", "%{dependency_file}"],
                            expand_if_available = "dependency_file",
                        ),
                    ],
                ),
            ],
        ),
        feature(
            name = "default_compile_flags",
            flag_sets = [
                flag_set(
                    actions = [
                        ACTION_NAMES.assemble,
                        ACTION_NAMES.preprocess_assemble,
                        ACTION_NAMES.linkstamp_compile,
                        ACTION_NAMES.c_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_header_parsing,
                        ACTION_NAMES.cpp_module_compile,
                        ACTION_NAMES.cpp_module_codegen,
                        ACTION_NAMES.lto_backend,
                        ACTION_NAMES.clif_match,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "--target=" + target_system_name,
                                "-Wall",
                                "-Werror",
                                "-Wextra-semi",
                                "-Wnewline-eof",
                                "-Wshadow",
                                "-fdiagnostics-color",
                                "-ffuchsia-api-level=" + str(DEFAULT_CLANG_TARGET_API),
                            ],
                        ),
                    ],
                ),
                flag_set(
                    actions = [
                        ACTION_NAMES.linkstamp_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_header_parsing,
                        ACTION_NAMES.cpp_module_compile,
                        ACTION_NAMES.cpp_module_codegen,
                        ACTION_NAMES.lto_backend,
                        ACTION_NAMES.clif_match,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "-std=c++17",
                                "-xc++",
                                # Needed to compile shared libraries.
                                "-fPIC",
                            ],
                        ),
                    ],
                ),
            ],
            enabled = True,
        ),
        feature(
            name = "default_link_flags",
            flag_sets = [
                flag_set(
                    actions = [
                        ACTION_NAMES.cpp_link_executable,
                        ACTION_NAMES.cpp_link_dynamic_library,
                        ACTION_NAMES.cpp_link_nodeps_dynamic_library,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "--target=" + target_system_name,
                                "--driver-mode=g++",
                                "-lzircon",
                            ],
                        ),
                    ],
                ),
            ],
            enabled = True,
        ),
        feature(
            name = "supports_pic",
            enabled = True,
        ),
        feature(
            name = "coverage",
            flag_sets = [
                flag_set(
                    actions = [
                        ACTION_NAMES.c_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_link_dynamic_library,
                        ACTION_NAMES.cpp_link_executable,
                        ACTION_NAMES.cpp_link_nodeps_dynamic_library,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "-fprofile-instr-generate",
                                "-fcoverage-mapping",
                            ],
                        ),
                    ],
                ),
                flag_set(
                    actions = [
                        ACTION_NAMES.c_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_module_compile,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "-O1",
                            ],
                        ),
                    ],
                ),
                flag_set(
                    actions = [
                        ACTION_NAMES.cpp_link_dynamic_library,
                        ACTION_NAMES.cpp_link_executable,
                        ACTION_NAMES.cpp_link_nodeps_dynamic_library,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                # The statically-linked profiling runtime depends on libzircon.
                                "-lzircon",
                            ],
                        ),
                    ],
                ),
            ],
        ),
        feature(
            name = "ml_inliner",
            flag_sets = [
                flag_set(
                    actions = [
                        ACTION_NAMES.c_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_module_compile,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "-mllvm",
                                "-enable-ml-inliner=release",
                            ],
                        ),
                    ],
                    with_features = [with_feature_set(
                        features = ["opt"],
                    )],
                ),
            ],
        ),
        feature(
            name = "sanitizer",
            flag_sets = [
                flag_set(
                    actions = [
                        ACTION_NAMES.c_compile,
                        ACTION_NAMES.cpp_compile,
                        ACTION_NAMES.cpp_module_compile,
                    ],
                    flag_groups = [
                        flag_group(
                            flags = [
                                "-fno-omit-frame-pointer",
                                "-g3",
                                "-O1",
                            ],
                        ),
                    ],
                ),
            ],
        ),
        _sanitizer_feature("asan", "address"),
        _sanitizer_feature("lsan", "leak"),
        _sanitizer_feature("msan", "memory"),
        _sanitizer_feature("tsan", "thread"),
        _sanitizer_feature("ubsan", "undefined"),
    ]
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
            "%{CROSSTOOL_ROOT}/include/" + ctx.attr.cpu + "-unknown-fuchsia/c++/v1",  # Platform libc++.
            "%{CROSSTOOL_ROOT}/include/c++/v1",  # Platform libc++.
            "%{CROSSTOOL_ROOT}/lib/clang/%{CLANG_VERSION}/include",  # Platform libc++.
            "%{CROSSTOOL_ROOT}/lib/clang/%{CLANG_VERSION}/share",  # Platform libc++.
        ],
        builtin_sysroot = "%{SYSROOT_PATH_PREFIX}" + ctx.attr.cpu,
        features = features,
        cc_target_os = "fuchsia",
    )

cc_toolchain_config = rule(
    implementation = _cc_toolchain_config_impl,
    attrs = {
        "cpu": attr.string(mandatory = True, values = ["aarch64", "x86_64"]),
    },
    provides = [CcToolchainConfigInfo],
)

def _feature_flag(ctx):
    toolchain = find_cpp_toolchain(ctx)
    feature = ctx.attr.feature_name
    feature_configuration = cc_common.configure_features(
        ctx = ctx,
        cc_toolchain = toolchain,
        requested_features = ctx.features,
        unsupported_features = ctx.disabled_features,
    )
    return [config_common.FeatureFlagInfo(value = str(cc_common.is_enabled(
        feature_configuration = feature_configuration,
        feature_name = feature,
    )))]

# A flag whose value corresponds to whether a toolchain feature is enabled.
# For instance if `feature_name = "asan"` and you passed `--features=asan`
# then the value will be True.
feature_flag = rule(
    implementation = _feature_flag,
    attrs = {
        "feature_name": attr.string(),
        "_cc_toolchain": attr.label(default = Label("@bazel_tools//tools/cpp:current_cc_toolchain")),
    },
    fragments = ["cpp"],
    toolchains = use_cpp_toolchain(),
)
