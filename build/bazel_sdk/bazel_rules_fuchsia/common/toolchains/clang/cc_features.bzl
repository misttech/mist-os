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
load("//common:toolchains/clang/clang_utils.bzl", "to_clang_target_tuple")

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

_all_conly_compile_actions = [
    ACTION_NAMES.c_compile,
    ACTION_NAMES.objc_compile,
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

def _make_flag_config(
        *,
        cflags = [],
        conlyflags = [],
        ccflags = [],
        ldflags = [],
        lld_flags = [],
        ld64_flags = [],
        combine_cflags_with_ldflags = True):
    """ Create a struct holding all compiler and linker flags.

    This struct can be seen as a GN config, grouping compiler and linker
    flags together,

    The method _iter_config_flags below can be used to extract the
    corresponding flag_group() lists from an input list of struct values.

    Args:
       cflags (string list, optional): Common C and C++ compiler flags.
       ccflags (string list, optional): C++ only compiler flags.
       conlyflags (string list, optional): C compiler flags (omitted from C++ compilations).
       ldflags (string list, optional): linker flags.

       lld_flags (string list, optional): linker flags to be used only
          when the Clang lld linker is used (typically on Linux).

       ld64_flags (string list, optional): linker flags to be used only
          when the MacOS ld64 linker is used.

       combine_cflags_with_ldflags (bool, optional): if True (the default), the values
          in cflags will be prepended to ldflags as well.

    Returns:
       A new struct with cflags, ccflags, ldflags keys, and whose values are either None
       of list of Bazel flag_group() values.
    """

    def _flag_group_or_none(flags):
        return flag_group(flags = flags) if len(flags) > 0 else None

    return struct(
        cflags = _flag_group_or_none(cflags),
        conlyflags = _flag_group_or_none(conlyflags),
        ccflags = _flag_group_or_none(ccflags),
        ldflags = _flag_group_or_none(
            (cflags if combine_cflags_with_ldflags else []) + ldflags,
        ),
        lld_flags = _flag_group_or_none(lld_flags),
        ld64_flags = _flag_group_or_none(ld64_flags),
    )

def _iter_config_flags(flag_name, flag_configs):
    """ Iterate over a list of config structs, and return the values for a given named flag.

    Args:
       flag_name: A flag name (e.g. "cflags", "ccflags", etc).
       flag_configs: A list of structs returned by _make_flag_config.
    Returns:
       A list of flag_group() values from the input configs's flag_name fields.
    """
    return [getattr(f, flag_name) for f in flag_configs if getattr(f, flag_name) != None]

def _iter_cflags(flag_configs):
    return _iter_config_flags("cflags", flag_configs)

def _iter_ccflags(flag_configs):
    return _iter_config_flags("ccflags", flag_configs)

def _iter_conlyflags(flag_configs):
    return _iter_config_flags("conlyflags", flag_configs)

def _iter_ldflags(flag_configs, use_ld64 = False):
    return (
        _iter_config_flags("ldflags", flag_configs) +
        _iter_config_flags(
            "ld64_flags" if use_ld64 else "lld_flags",
            flag_configs,
        )
    )

def _apply_if(feature):
    """Generate `with_features` value that matches a given feature name.

    This is useful to define flag_set() values that are only applied when a
    specific feature is enabled. Example usage:

        flag_set(
            actions = _all_compile_actions,
            flag_groups = [ ... ],
            with_features = _apply_if("dbg"),
        ),

    Args:
        feature (string): Feature flag name.
    Returns:
        A list of one with_feature_set() value matching |feature|.
    """
    return [with_feature_set(
        features = [feature],
    )]

# A global struct providing various constant flag configs that
# do not depend on either the host or target os/cpu values.
_flag_configs = struct(
    color_diagnostics = _make_flag_config(
        cflags = ["-fcolor-diagnostics"],
        ldflags = ["-Wl,--color-diagnostics"],
    ),
    pic = _make_flag_config(
        cflags = ["-fPIC"],
    ),
    language_cxx17 = _make_flag_config(
        ccflags = ["-std=c++17"],
    ),
    language_cxx20 = _make_flag_config(
        ccflags = ["-std=c++20"],
    ),
    no_frame_pointers = _make_flag_config(
        cflags = ["-fomit-frame-pointer"],
    ),
    linker_gc = _make_flag_config(
        cflags = [
            "-fdata-sections",
            "-ffunction-sections",
        ],
        lld_flags = ["-Wl,--gc-sections"],
    ),
    optimize_none = _make_flag_config(
        cflags = ["-O0"],
    ),
    optimize_debug = _make_flag_config(
        cflags = ["-Og"],
    ),
    optimize_default = _make_flag_config(
        cflags = ["-O2"],
    ),
    optimize_size = _make_flag_config(
        cflags = ["-Os"],
        ldflags = ["-Wl,-O2"],
    ),
    debuginfo = _make_flag_config(
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
    default_warnings = _make_flag_config(
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
    werror = _make_flag_config(
        cflags = [
            "-Werror",
            "-Wa,--fatal-warnings",
        ],
    ),
    no_exceptions = _make_flag_config(
        ccflags = ["-fno-exceptions"],
        ldflags = ["-fno-exceptions"],
    ),
    no_rtti = _make_flag_config(
        ccflags = ["-fno-rtti"],
        ldflags = ["-fno-rtti"],
    ),
    symbol_visibility_hidden = _make_flag_config(
        cflags = ["-fvisibility=hidden"],
        ccflags = ["-fvisibility-inlines-hidden"],
        combine_cflags_with_ldflags = False,
    ),
    release = _make_flag_config(
        cflags = ["-DNDEBUG=1"],
        combine_cflags_with_ldflags = False,
    ),
    link_zircon = _make_flag_config(
        ldflags = ["-lzircon"],
    ),
    driver_mode = _make_flag_config(
        ldflags = ["--driver-mode=g++"],
    ),
    symbol_no_undefined = _make_flag_config(
        lld_flags = ["-Wl,--no-undefined"],
        ld64_flags = ["-Wl,-undefined,error"],
    ),
    lto = _make_flag_config(
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
    icf = _make_flag_config(
        ldflags = ["-Wl,--icf=all"],
    ),
    ffp_contract_off = _make_flag_config(
        cflags = ["-ffp-contract=off"],
        combine_cflags_with_ldflags = False,
    ),
    auto_var_init = _make_flag_config(
        cflags = ["-ftrivial-auto-var-init=pattern"],
        combine_cflags_with_ldflags = False,
    ),
    relpath_debug_info = _make_flag_config(
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
        combine_cflags_with_ldflags = False,
    ),
    thread_safety_annotations = _make_flag_config(
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

# This is a special feature in that Bazel will put all of these flags first
def get_default_compile_flags_feature(
        clang_info,
        toolchain_repo_name,
        target_os,
        target_cpu,
        sysroot = ""):
    """Compute the special "default_compile_flags" feature().

    This feature is special because Bazel will place all its flags before all
    others in corresponding actions.

    Args:
       clang_info: A ClangInfo provider value.
       toolchain_repo_name: Canonical name of toolchain repository.
       target_os: Target OS, following Fuchsia conventions.
       target_cpu: Target CPU, following Fuchsia conventions.
       sysroot: Optional path to sysroot to use.

    Returns:
       A new feature() value.
    """
    host_os = clang_info.fuchsia_host_os
    host_cpu = clang_info.fuchsia_host_arch

    is_macos = target_os == "mac"

    clang_tuple = to_clang_target_tuple(target_os, target_cpu)
    toolchain_dir = "external/{}".format(toolchain_repo_name)
    macos_sdk_path = "{}/xcode/MacSDK".format(toolchain_dir)
    clang_version = clang_info.short_version

    # On MacOS, the ld64 linker is required, which uses slightly
    # different flags for certain features.
    use_ld64 = is_macos

    default_cflags = []
    default_conlyflags = []
    default_ccflags = []
    default_ldflags = []

    # Include search paths for libc++, which uses #include_next directives, expecting
    # headers to be listed in a very specific order.
    if target_os == "mac":
        default_cflags += [
            # Do not use built-in include search paths. This prevents
            # picking system-installed headers or libraries by mistake.
            # This flag also makes --sysroot a no-op!!
            "-nostdinc",

            # Ensure system framework headers are picked from the Mac SDK
            "-iframework",
            "{}/Frameworks".format(macos_sdk_path),
        ]

        # See https://skia.googlesource.com/skia/+/620de5ac9f6b/toolchain/mac_toolchain_config.bzl
        default_conlyflags += [
            # Access C library headers.
            "-isystem",
            "{}/usr/include".format(macos_sdk_path),
        ]
        default_ccflags += [
            # Ensure the toolchain's libc++ headers are picked up at compilation time,
            # before the ones that come from the Mac SDK.
            "-isystem",
            "{}/include/c++/v1".format(toolchain_dir),
            "-isystem",
            "{}/lib/clang/{}/include".format(toolchain_dir, clang_info.short_version),
            "-isystem",
            "{}/usr/include".format(macos_sdk_path),
        ]
        default_ldflags += [
            # Ensure system framework libraries are picked from the Mac SDK
            "-iframework",
            "{}/Frameworks".format(macos_sdk_path),
            # Ensure that our toolchain's libc++ host runtime is used at link time.
            # This must appear before xcode/MacSDK/usr/lib below to ensure the XCode
            # libc++ is not picked up by mistake.
            "-Wl,-L,{}/lib".format(toolchain_dir),
            "-lc++",
            # Ensure the standard C library runtime is found at link time.
            "-Wl,-L,{}/usr/lib".format(macos_sdk_path),
            # Debugging help (uncomment to enable)
            # "-Wl,--verbose",
        ]
    else:  # Fuchsia or Linux
        if sysroot:
            default_cflags += [
                "--sysroot={}".format(sysroot),
            ]
            default_ldflags += [
                "--sysroot={}".format(sysroot),
            ]

    default_system_flags = _make_flag_config(
        cflags = default_cflags,
        conlyflags = default_conlyflags,
        ccflags = default_ccflags,
        ldflags = default_ldflags,
        combine_cflags_with_ldflags = False,
    )

    return feature(
        name = "default_compile_flags",
        flag_sets = [
            # These are cflags that will be added to all builds
            flag_set(
                actions = _all_compile_actions,
                flag_groups = _iter_cflags([
                    default_system_flags,
                    _flag_configs.color_diagnostics,
                    _flag_configs.pic,
                    _flag_configs.linker_gc,
                    _flag_configs.no_frame_pointers,
                    _flag_configs.debuginfo,
                    _flag_configs.default_warnings,
                    _flag_configs.werror,
                    _flag_configs.symbol_visibility_hidden,
                    _flag_configs.ffp_contract_off,
                    _flag_configs.auto_var_init,
                    _flag_configs.thread_safety_annotations,
                    _flag_configs.relpath_debug_info,
                ]),
            ),
            # These are conlyflags that will be added to all builds
            flag_set(
                actions = _all_conly_compile_actions,
                flag_groups = _iter_conlyflags([
                    default_system_flags,
                ]),
            ),
            # These are ccflags that will be added to all builds
            flag_set(
                actions = _all_cpp_compile_actions,
                flag_groups = _iter_ccflags([
                    default_system_flags,
                    _flag_configs.language_cxx20,
                    _flag_configs.no_exceptions,
                    _flag_configs.no_rtti,
                    _flag_configs.symbol_visibility_hidden,
                    _flag_configs.relpath_debug_info,
                ]),
            ),
            # These are cflags that will be added to dbg builds
            flag_set(
                actions = _all_compile_actions,
                flag_groups = _iter_cflags([
                    _flag_configs.optimize_debug,
                ]),
                with_features = _apply_if("dbg"),
            ),
            # These are cflags that will be added to opt builds
            flag_set(
                actions = _all_compile_actions,
                flag_groups = _iter_cflags([
                    _flag_configs.optimize_size,
                    _flag_configs.release,
                    # TODO(b/299545705) turn on LTO for all opt builds
                    # _flag_configs.lto,
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
                    default_system_flags,
                    _flag_configs.driver_mode,
                    _flag_configs.color_diagnostics,
                    _flag_configs.no_frame_pointers,
                    _flag_configs.linker_gc,
                    _flag_configs.debuginfo,
                    _flag_configs.no_exceptions,
                    _flag_configs.no_rtti,
                    _flag_configs.pic,
                    _flag_configs.symbol_no_undefined,
                    _flag_configs.icf,
                    _flag_configs.relpath_debug_info,
                ], use_ld64 = use_ld64) + (
                    _iter_ldflags([
                        _flag_configs.link_zircon,
                    ], use_ld64 = False) if target_os == "fuchsia" else []
                ),
            ),
            # These are ldflags that will be added to dbg builds
            flag_set(
                actions = _all_link_actions,
                flag_groups = _iter_ldflags([
                    _flag_configs.optimize_debug,
                ], use_ld64 = use_ld64),
                with_features = _apply_if("dbg"),
            ),
            # These are ldflags that will be added to opt builds
            flag_set(
                actions = _all_link_actions,
                flag_groups = _iter_ldflags([
                    _flag_configs.optimize_size,
                    # TODO(b/299545705) turn on LTO for all opt builds
                    # _flag_configs.lto,
                ], use_ld64 = use_ld64),
                with_features = _apply_if("opt"),
            ),
        ],
        enabled = True,
        implies = [
            # LINT.IfChange(target_system_name)
            "target_system_name",
            # LINT.ThenChange(toolchain_utils.bzl)
        ],
    )

action_names = struct(
    all_actions = _all_actions,
    all_compile_actions = _all_compile_actions,
    all_conly_compile_actions = _all_conly_compile_actions,
    all_cpp_compile_actions = _all_cpp_compile_actions,
    all_link_actions = _all_link_actions,
)
