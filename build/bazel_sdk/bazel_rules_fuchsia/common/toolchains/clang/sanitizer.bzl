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

sanitizer_features = [
    sanitizer_feature,
    _sanitizer_feature("asan", "address"),
    _sanitizer_feature("lsan", "leak"),
    _sanitizer_feature("msan", "memory"),
    _sanitizer_feature("tsan", "thread"),
    _sanitizer_feature("ubsan", "undefined"),
]
