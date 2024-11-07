# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Clang C++ toolchain feature definitions."""

load("@bazel_tools//tools/build_defs/cc:action_names.bzl", "ACTION_NAMES")
load(
    "@bazel_tools//tools/cpp:cc_toolchain_config_lib.bzl",
    "feature",
    "flag_group",
    "flag_set",
    "with_feature_set",
)

# buildifier: disable=unused-variable
_all_actions = [
    ACTION_NAMES.assemble,
    ACTION_NAMES.preprocess_assemble,
    ACTION_NAMES.c_compile,
    ACTION_NAMES.cpp_compile,
    ACTION_NAMES.cpp_module_compile,
    ACTION_NAMES.objc_compile,
    ACTION_NAMES.objcpp_compile,
    ACTION_NAMES.cpp_header_parsing,
    ACTION_NAMES.clif_match,
]

_all_compile_actions = [
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
]

_all_cpp_compile_actions = [
    ACTION_NAMES.linkstamp_compile,
    ACTION_NAMES.cpp_compile,
    ACTION_NAMES.cpp_header_parsing,
    ACTION_NAMES.cpp_module_compile,
    ACTION_NAMES.cpp_module_codegen,
    ACTION_NAMES.lto_backend,
    ACTION_NAMES.clif_match,
]

_all_link_actions = [
    ACTION_NAMES.cpp_link_executable,
    ACTION_NAMES.cpp_link_dynamic_library,
    ACTION_NAMES.cpp_link_nodeps_dynamic_library,
]

def _apply_if(feature):
    """ helper function to make things less verbose """
    return [with_feature_set(
        features = [feature],
    )]

def _iter_cflags(flag_groups):
    """ Iterate over all of the cflags in the flag_group"""
    return [f.cflags for f in flag_groups]

def _iter_ccflags(flag_groups):
    """ Iterate over all of the ccflags in the flag_group"""
    return [f.ccflags for f in flag_groups]

def _iter_ldflags(flag_groups):
    """ Iterate over all of the ldflags in the flag_group"""
    return [f.ldflags for f in flag_groups]

def _make_flag_group_struct(*, cflags = [], ccflags = [], ldflags = [], combine_cflags_with_ldflags = True):
    """ Create a struct holding all of the common flags.

    The cflags will be folded into the ldflags unless combine_cflags_with_ldflags is False.
    """

    def _flag_group_or_none(flags):
        return flag_group(flags = flags) if len(flags) > 0 else None

    return struct(
        cflags = _flag_group_or_none(cflags),
        ccflags = _flag_group_or_none(ccflags),
        ldflags = _flag_group_or_none(
            (cflags if combine_cflags_with_ldflags else []) + ldflags,
        ),
    )

_flag_groups = struct(
    color_diagnostics = _make_flag_group_struct(
        cflags = ["-fcolor-diagnostics"],
        ldflags = ["-Wl,--color-diagnostics"],
    ),
    pic = _make_flag_group_struct(
        cflags = ["-fPIC"],
    ),
    language = _make_flag_group_struct(
        ccflags = ["-std=c++20"],
    ),
    no_frame_pointers = _make_flag_group_struct(
        cflags = ["-fomit-frame-pointer"],
    ),
    linker_gc = _make_flag_group_struct(
        cflags = [
            "-fdata-sections",
            "-ffunction-sections",
        ],
        ldflags = ["-Wl,--gc-sections"],
    ),
    optimize_none = _make_flag_group_struct(
        cflags = ["-O0"],
    ),
    optimize_debug = _make_flag_group_struct(
        cflags = ["-Og"],
    ),
    optimize_default = _make_flag_group_struct(
        cflags = ["-O2"],
    ),
    optimize_size = _make_flag_group_struct(
        cflags = ["-Os"],
        ldflags = ["-Wl,-O2"],
    ),
    debuginfo = _make_flag_group_struct(
        cflags = [
            "-g3",
            "-gdwarf-5",
            "-gz=zstd",
            "-Xclang",
            "-debug-info-kind=constructor",
        ],
        ldflags = [
            "-g3",
            "-gdwarf-5",
            "-gz=zstd",
        ],
        combine_cflags_with_ldflags = False,
    ),
    default_warnings = _make_flag_group_struct(
        cflags = [
            "-Wall",
            "-Wextra-semi",
            "-Wextra",
            "-Wnewline-eof",
            "-Wno-missing-field-initializers",
            "-Wno-sign-conversion",
            "-Wno-unused-parameter",
            "-Wnonportable-system-include-path",
            # TODO(b/315062126) Some in-tree builds are failing because we
            # are shadowing variables.
            #"-Wshadow",
            "-Wstrict-prototypes",
            "-Wwrite-strings",
            "-Wthread-safety",
            # TODO(https://fxbug.dev/344080745): After the issue is fixed,
            # remove "-Wno-missing-template-arg-list-after-template-kw".
            "-Wno-unknown-warning-option",
            "-Wno-missing-template-arg-list-after-template-kw",
        ],
    ),
    werror = _make_flag_group_struct(
        cflags = [
            "-Werror",
            "-Wa,--fatal-warnings",
        ],
    ),
    no_exceptions = _make_flag_group_struct(
        ccflags = ["-fno-exceptions"],
        ldflags = ["-fno-exceptions"],
    ),
    no_rtti = _make_flag_group_struct(
        ccflags = ["-fno-rtti"],
        ldflags = ["-fno-rtti"],
    ),
    symbol_visibility_hidden = _make_flag_group_struct(
        cflags = ["-fvisibility=hidden"],
        ccflags = ["-fvisibility-inlines-hidden"],
        combine_cflags_with_ldflags = False,
    ),
    release = _make_flag_group_struct(
        cflags = ["-DNDEBUG=1"],
        combine_cflags_with_ldflags = False,
    ),
    link_zircon = _make_flag_group_struct(
        ldflags = ["-lzircon"],
    ),
    driver_mode = _make_flag_group_struct(
        ldflags = ["--driver-mode=g++"],
    ),
    symbol_no_undefined = _make_flag_group_struct(
        ldflags = ["-Wl,--no-undefined"],
    ),
    lto = _make_flag_group_struct(
        cflags = [
            "-flto",
            "-fwhole-program-vtables",
            "-mllvm",
            "-wholeprogramdevirt-branch-funnel-threshold=0",
        ],
        ldflags = [
            "-flto",
            "-fwhole-program-vtables",
            "-Wl,-mllvm,--wholeprogramdevirt-branch-funnel-threshold=0",
        ],
        combine_cflags_with_ldflags = False,
    ),
    icf = _make_flag_group_struct(
        ldflags = ["-Wl,--icf=all"],
    ),
    ffp_contract_off = _make_flag_group_struct(
        cflags = ["-ffp-contract=off"],
        combine_cflags_with_ldflags = False,
    ),
    auto_var_init = _make_flag_group_struct(
        cflags = ["-ftrivial-auto-var-init=pattern"],
        combine_cflags_with_ldflags = False,
    ),
    relpath_debug_info = _make_flag_group_struct(
        # Relativize paths to source files and linker inputs to avoid
        # leaking absolute paths, and ensure consistency
        # between local and remote compiling/linking.
        cflags = [
            "-ffile-compilation-dir=.",
            "-no-canonical-prefixes",
        ],
        ccflags = [
            "-ffile-compilation-dir=.",
            "-no-canonical-prefixes",
        ],
        ldflags = [
            "-no-canonical-prefixes",
        ],
    ),
    thread_safety_annotations = _make_flag_group_struct(
        cflags = [
            "-Wthread-safety",

            # TODO(https://fxbug.dev/42085252): Clang is catching instances of these in the kernel and drivers.
            # Temporarily disable them for now to facilitate the roll then come back and
            # fix them.
            "-Wno-unknown-warning-option",
            "-Wno-thread-safety-reference-return",
            "-D_LIBCPP_ENABLE_THREAD_SAFETY_ANNOTATIONS=1",
        ],
        combine_cflags_with_ldflags = False,
    ),
)

#
## Begin feature definitions
#

def _target_system_name_feature(target_system_name):
    return feature(
        name = "fuchsia_target_system_name",
        flag_sets = [
            flag_set(
                actions = _all_compile_actions + _all_link_actions,
                flag_groups = [
                    flag_group(
                        flags = ["--target=" + target_system_name],
                    ),
                ],
            ),
        ],
    )

# This is a special feature in that Bazel will put all of these flags first
_default_compile_flags_feature = feature(
    name = "default_compile_flags",
    flag_sets = [
        # These are cflags that will be added to all builds
        flag_set(
            actions = _all_compile_actions,
            flag_groups = _iter_cflags([
                _flag_groups.color_diagnostics,
                _flag_groups.pic,
                _flag_groups.linker_gc,
                _flag_groups.no_frame_pointers,
                _flag_groups.debuginfo,
                _flag_groups.default_warnings,
                _flag_groups.werror,
                _flag_groups.symbol_visibility_hidden,
                _flag_groups.ffp_contract_off,
                _flag_groups.auto_var_init,
                _flag_groups.thread_safety_annotations,
                _flag_groups.relpath_debug_info,
            ]),
        ),
        # These are ccflags that will be added to all builds
        flag_set(
            actions = _all_cpp_compile_actions,
            flag_groups = _iter_ccflags([
                _flag_groups.language,
                _flag_groups.no_exceptions,
                _flag_groups.no_rtti,
                _flag_groups.symbol_visibility_hidden,
                _flag_groups.relpath_debug_info,
            ]),
        ),
        # These are cflags that will be added to dbg builds
        flag_set(
            actions = _all_compile_actions,
            flag_groups = _iter_cflags([
                _flag_groups.optimize_debug,
            ]),
            with_features = _apply_if("dbg"),
        ),
        # These are cflags that will be added to opt builds
        flag_set(
            actions = _all_compile_actions,
            flag_groups = _iter_cflags([
                _flag_groups.optimize_size,
                _flag_groups.release,
                # TODO(b/299545705) turn on LTO for all opt builds
                # _flag_groups.lto,
            ]),
            with_features = _apply_if("opt"),
        ),

        # Begin link Actions:
        # Note: The link actions must be added to the 'default_compile_flags' feature.
        # Bazel will move all of these to the top of the command linke which makes it
        # possible for users to override certain flags.

        # These are ldflags that are applied to all builds
        flag_set(
            actions = _all_link_actions,
            flag_groups = _iter_ldflags([
                _flag_groups.driver_mode,
                _flag_groups.link_zircon,
                _flag_groups.color_diagnostics,
                _flag_groups.no_frame_pointers,
                _flag_groups.linker_gc,
                _flag_groups.debuginfo,
                _flag_groups.no_exceptions,
                _flag_groups.no_rtti,
                _flag_groups.pic,
                _flag_groups.symbol_no_undefined,
                _flag_groups.icf,
                _flag_groups.relpath_debug_info,
            ]),
        ),
        # These are ldflags that will be added to dbg builds
        flag_set(
            actions = _all_link_actions,
            flag_groups = _iter_ldflags([
                _flag_groups.optimize_debug,
            ]),
            with_features = _apply_if("dbg"),
        ),
        # These are ldflags that will be added to opt builds
        flag_set(
            actions = _all_link_actions,
            flag_groups = _iter_ldflags([
                _flag_groups.optimize_size,
                # TODO(b/299545705) turn on LTO for all opt builds
                # _flag_groups.lto,
            ]),
            with_features = _apply_if("opt"),
        ),
    ],
    enabled = True,
    implies = [
        "fuchsia_target_system_name",
    ],
)

_supports_pic_feature = feature(
    name = "supports_pic",
    enabled = True,
)

# This feature adds an RPATH entry into the final binary. We do not want this
# because it is not valid for fuchsia since we install all of our libraries
# in /lib of our package. Enabling this just adds size to our binaries.
_no_runtime_library_search_directories_feature = feature(
    name = "runtime_library_search_directories",
    enabled = False,
)

features = struct(
    default_compile_flags = _default_compile_flags_feature,
    target_system_name = _target_system_name_feature,
    supports_pic = _supports_pic_feature,
    no_runtime_library_search_directories = _no_runtime_library_search_directories_feature,
)
