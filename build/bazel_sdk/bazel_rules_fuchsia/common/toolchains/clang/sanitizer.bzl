# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Sanitizers definitions for Clang.
"""

load("@bazel_tools//tools/build_defs/cc:action_names.bzl", "ACTION_NAMES")
load(
    "@bazel_tools//tools/cpp:cc_toolchain_config_lib.bzl",
    "feature",
    "flag_group",
    "flag_set",
)

sanitizer_feature = feature(
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
)

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

def _sanitizer_feature_ext(name, compile_fsanitize, link_fsanitize):
    return feature(
        name = name,
        flag_sets = [
            flag_set(
                actions = [
                    ACTION_NAMES.c_compile,
                    ACTION_NAMES.cpp_compile,
                    ACTION_NAMES.cpp_module_compile,
                ],
                flag_groups = [flag_group(flags = ["-fsanitize=" + compile_fsanitize])],
            ),
            flag_set(
                actions = [
                    ACTION_NAMES.cpp_link_executable,
                    ACTION_NAMES.cpp_link_dynamic_library,
                    ACTION_NAMES.cpp_link_nodeps_dynamic_library,
                ],
                flag_groups = [flag_group(flags = ["-fsanitize=" + link_fsanitize])],
            ),
        ],
        implies = ["sanitizer"],
    )

sanitizer_features = [
    sanitizer_feature,
    # IMPORTANT: When multiple features are enabled, their corresponding
    # compiler/linker flags will appear in the order specified in this
    # list.
    #
    # It is critical that "asan" appears before "hwasan", because the
    # latter overrides it. The definitions of the @fuchsia_clang
    # :asan_variant and :hwasan_variant config settings should reflect
    # that too!
    #
    # In other words, using `--features=asan --features=hwasan`
    # enables hwasan only, but so does `--features=hwasan --features=asan`
    # since there is no way to know in which order the features were
    # passed on the command-line.
    #
    # lsan is anothe special case because the compiler and linker flags
    # are different (the lsan runtime code in provided by the asan library).
    # Also -fsanitize=address will override -fsanitize=leak.
    #
    _sanitizer_feature("ubsan", "undefined"),
    _sanitizer_feature_ext("lsan", "leak", "address"),
    _sanitizer_feature("asan", "address"),
    _sanitizer_feature("hwasan", "hwaddress"),
    _sanitizer_feature("msan", "memory"),
    _sanitizer_feature("tsan", "thread"),
]
