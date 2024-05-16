# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities for extracting, creating, and manipulating debug symbols."""

load("@rules_cc//cc:find_cc_toolchain.bzl", "find_cc_toolchain")
load(
    ":providers.bzl",
    "FuchsiaCollectedUnstrippedBinariesInfo",
    "FuchsiaDebugSymbolInfo",
    "FuchsiaPackageResourcesInfo",
    "FuchsiaUnstrippedBinaryInfo",
)
load(":utils.bzl", "flatten", "make_resource_struct")

FUCHSIA_DEBUG_SYMBOLS_ATTRS = {
    "_elf_strip_tool": attr.label(
        default = "//fuchsia/tools:elf_strip",
        executable = True,
        cfg = "exec",
    ),
    "_generate_symbols_dir_tool": attr.label(
        default = "//fuchsia/tools:generate_symbols_dir",
        executable = True,
        cfg = "exec",
    ),
    "_cc_toolchain": attr.label(
        default = Label("@bazel_tools//tools/cpp:current_cc_toolchain"),
    ),
}

def strip_resources(ctx, resources, build_id_path = None, source_dir = None):
    """Generate an action to strip resources.

    The generated action will output a single .build-id directory which will contain
    symlinks to all unstripped ELF binaries from the given resources. This action will
    always generate a directory even if there are no resources to strip.

    In addition, the action will generate a file in the build id directory named
    .stamp which will contain the full names of all of the debug symbols that were
    generated.

    Args:
      ctx: rule context.
      resources: A list of unstripped input resource_struct() values.
      build_id_path: (optional) A string which will be used when declaring
        the build id directory. Defaults to `ctx.label.name + "/.build-id"`.
      source_dir: (optional) Either a File value that points to a directory (or file),
        used by the debugger to locate source files, or a string naming an environment
        variable (see FuchsiaDebugSymbolInfo documentation).

    Returns:
      a pair whose first item is a list of stripped resource_struct() instances,
      and the second item is a FuchsiaDebugSymbolInfo provider for the
      corresponding .build-id directory.
    """

    build_id_path = build_id_path or (ctx.label.name + "/.build-id")

    if type(build_id_path) != "string":
        fail("'{}' must be a string but got {}.".format(build_id_path, type(build_id_path)))

    build_id_dir = ctx.actions.declare_directory(build_id_path)

    stripped_resources = []
    all_maybe_elf_files = []
    all_ids_txt = []

    # We need to make sure we have a unique set of inputs. If we have duplicate
    # resources then the ctx.actions.declare_file below will fail because it
    # will try to declare the same file twice. We only need to strip the resource
    # once so there is no need to attempt to stip duplicates.
    for r in depset(resources).to_list():
        ids_txt = ctx.actions.declare_file(r.src.path + "ids_txt")
        all_ids_txt.append(ids_txt)
        all_maybe_elf_files.append(r.src)
        stripped_resources.append(_maybe_process_elf(ctx, r, ids_txt))

    ctx.actions.run(
        executable = ctx.executable._generate_symbols_dir_tool,
        arguments = [build_id_dir.path] + [f.path for f in all_ids_txt],
        outputs = [build_id_dir],
        inputs = all_ids_txt + all_maybe_elf_files,
        mnemonic = "GenerateDebugSymbols",
        progress_message = "Generate dir with debug symbols for %s" % ctx.label,
    )

    if source_dir == None:
        source_dir = "BUILD_WORKSPACE_DIRECTORY"
    elif type(source_dir) not in ("File", "string"):
        fail("The 'source_dir' argument should be a string or a File value, got: %s" % repr(source_dir))

    return stripped_resources, FuchsiaDebugSymbolInfo(build_id_dirs = {
        source_dir: depset([build_id_dir]),
    })

def _maybe_process_elf(ctx, r, ids_txt):
    cc_toolchain = find_cc_toolchain(ctx)
    stripped = ctx.actions.declare_file(r.src.path + "_stripped")

    ctx.actions.run(
        outputs = [stripped, ids_txt],
        inputs = [r.src],
        tools = cc_toolchain.all_files,
        executable = ctx.executable._elf_strip_tool,
        progress_message = "Extracting debug symbols from %s" % r.src,
        mnemonic = "ExtractDebugFromELF",
        arguments = [
            cc_toolchain.objcopy_executable,
            r.src.path,
            stripped.path,
            ids_txt.path,
        ],
    )

    return make_resource_struct(
        src = stripped,
        dest = r.dest,
    )

def _fuchsia_debug_symbols_impl(ctx):
    return [
        FuchsiaDebugSymbolInfo(build_id_dirs = {
            ctx.file.build_dir: depset(transitive = [
                target[DefaultInfo].files
                for target in ctx.attr.build_id_dirs
            ]),
        }),
    ]

fuchsia_debug_symbols = rule(
    doc = """Rule-based constructor for FuchsiaDebugSymbolInfo.""",
    implementation = _fuchsia_debug_symbols_impl,
    attrs = {
        "build_dir": attr.label(
            doc = "A direct file child within a build directory used by zxdb to locate code.",
            mandatory = True,
            allow_single_file = True,
        ),
        "build_id_dirs": attr.label_list(
            doc = "The build_id directories with symbols to be registered.",
            mandatory = True,
            allow_files = True,
        ),
    },
)

def collect_debug_symbols(*targets_or_providers):
    build_id_dirs = [
        (target_or_provider if (
            hasattr(target_or_provider, "build_id_dirs")
        ) else target_or_provider[FuchsiaDebugSymbolInfo]).build_id_dirs
        for target_or_provider in flatten(targets_or_providers)
        if hasattr(target_or_provider, "build_id_dirs") or FuchsiaDebugSymbolInfo in target_or_provider
    ]
    return FuchsiaDebugSymbolInfo(build_id_dirs = {
        build_dir: depset(transitive = [
            build_dir_mapping[build_dir]
            for build_dir_mapping in build_id_dirs
            if build_dir in build_dir_mapping
        ])
        for build_dir in depset([
            file
            for file in flatten([build_id_dir.keys() for build_id_dir in build_id_dirs])
            if type(file) == "File"
        ]).to_list() + depset([
            string
            for string in flatten([build_id_dir.keys() for build_id_dir in build_id_dirs])
            if type(string) == "string"
        ]).to_list()
    })

def get_build_id_dirs(debug_symbol_info):
    return flatten([depset.to_list() for depset in debug_symbol_info.build_id_dirs.values()])

def _fuchsia_unstripped_binary_impl(ctx):
    return FuchsiaUnstrippedBinaryInfo(
        dest = ctx.attr.dest,
        unstripped_file = ctx.file.unstripped_file,
        stripped_file = ctx.file.stripped_file if ctx.attr.stripped_file else None,
        source_dir = ctx.file.stripped_file if ctx.attr.source_dir else None,
    )

fuchsia_unstripped_binary = rule(
    doc = "Rule-based constructor for a FuchsiaUnstrippedBinaryInfo value.",
    implementation = _fuchsia_unstripped_binary_impl,
    attrs = {
        "dest": attr.string(
            doc = "Installation location in Fuchsia package for the stripped binary.",
            mandatory = True,
        ),
        "unstripped_file": attr.label(
            doc = "Unstripped ELF binary file",
            mandatory = True,
            allow_single_file = True,
        ),
        "stripped_file": attr.label(
            doc = "Optional stripped ELF binary file, if available as prebuilt.",
            mandatory = False,
            allow_single_file = True,
        ),
        "source_dir": attr.label(
            doc = "Optional label to source directory or file inside source directory.",
            mandatory = False,
            allow_single_file = True,
        ),
    },
)

def _convert_unstripped_binary_info(binary_info):
    """Convert a FuchsiaUnstrippedBinaryInfo into an equivalent FuchsiaCollectedUnstrippedBinariesInfo mapping value."""
    source_dir = binary_info.source_dir
    if source_dir == None:
        source_dir = "BUILD_WORKSPACE_DIRECTORY"
    return {
        source_dir: depset([
            struct(
                dest = binary_info.dest,
                unstripped_file = binary_info.unstripped_file,
                stripped_file = binary_info.stripped_file,
            ),
        ]),
    }

def _collect_unstripped_binaries_info(*targets_or_providers):
    """Merge any number of unstripped binary info providers or targets.

    Args:
       targets_or_providers: the list of arguments will be flattened, and items that
            are FuchsiaCollectedUnstrippedBinariesInfo providers, or targets that provide
            such values will be used as direct inputs for the merge. Items that
            are FuchsiaUnstrippedBinaryInfo or targets that provide such values will
            first be converted into a FuchsiaCollectedUnstrippedBinariesInfo value and
            the result will be used as input for the merge.
    Returns:
        A new FuchsiaCollectedUnstrippedBinariesInfo value, merging the content of
        the input arguments.
    """
    mappings = []
    for t in flatten(targets_or_providers):
        if hasattr(t, "source_dir_to_unstripped_binary"):
            mappings.append(t.source_dir_to_unstripped_binary)
        elif hasattr(t, "unstripped_binary") and hasattr(t, "dest"):
            mappings.append(_convert_unstripped_binary_info(t))
        elif type(t) == "Target":
            if FuchsiaCollectedUnstrippedBinariesInfo in t:
                mappings.append(t[FuchsiaCollectedUnstrippedBinariesInfo].source_dir_to_unstripped_binary)
            elif FuchsiaUnstrippedBinaryInfo in t:
                mappings.append(_convert_unstripped_binary_info(t[FuchsiaUnstrippedBinaryInfo]))
        else:
            fail("Invalid type {} of provider/target value {}".format(type(t), repr(t)))

    all_source_dirs = {
        source_dir: True
        for mapping in mappings
        for source_dir in mapping.keys()
    }
    return FuchsiaCollectedUnstrippedBinariesInfo(source_dir_to_unstripped_binary = {
        source_dir: depset(transitive = [
            mapping[source_dir]
            for mapping in mappings
            if source_dir in mapping
        ])
        for source_dir in all_source_dirs.keys()
    })

# A map of rule kind strings to tuples of attribute names for possible dependencies.
# Used by _get_target_deps_from_attributes() below.
_KNOWN_RULE_KINDS_TO_DEP_ATTR_NAMES = {
    "filegroup": ("data", "deps", "srcs"),
    "cc_binary": ("data", "deps", "srcs", "additional_linker_inputs", "dynamic_dep", "link_extra_libs", "malloc", "reexport_deps", "win_def_file"),
    "cc_import": ("data", "deps", "hdrs", "interface_library", "objects", "pic_objects", "pic_static_library", "shared_library", "static_library"),
    "cc_library": ("data", "deps", "srcs", "hdrs", "additional_compiler_inputs", "additional_linker_inputs", "implementation_deps", "linkstamp", "textual_hdrs", "win_def_file"),
    "cc_proto_library": ("deps",),
    "cc_shared_library": ("deps", "additional_linker_inputs", "dynamic_deps", "roots", "win_def_file"),
    "cc_test": ("deps", "srcs", "data", "additional_linker_inputs", "dynamic_deps", "link_extra_libs", "malloc", "reexport_deps", "win_def_file"),
}

def _get_target_deps_from_attributes(rule_attr, rule_kind = None):
    """Return all dependencies from a given target context during analysis.

    Args:
        rule_attr: The ctx.attr value for the current target.
        rule_kind: Optional string for the rule kind (this is aspect_ctx.rule.kind
             when called from an aspect implementation function). If provided,
             this can speed up the computation for a few known target kinds.
    Returns:
        A list of Target values corresponding to the dependencies of the current
        target.
    """
    attr_names = _KNOWN_RULE_KINDS_TO_DEP_ATTR_NAMES.get(rule_kind)
    if not attr_names:
        # For unknown rule kinds, parse all attributes and filter
        # those that are Targets or lists of Targets to the result.
        attr_names = dir(rule_attr)

    result = []
    for attr_name in attr_names:
        attr_value = getattr(rule_attr, attr_name, None)
        if not attr_value:
            continue
        if type(attr_value) == "Target":
            result.append(attr_value)
            continue
        if type(attr_value) == "list" and type(attr_value[0]) == "Target":
            result.extend(attr_value)
            continue

    return depset(result).to_list()

def _fuchsia_collect_unstripped_binaries_aspect_impl(target, aspect_ctx):
    return _collect_unstripped_binaries_info(
        target,
        _get_target_deps_from_attributes(aspect_ctx.rule.attr, aspect_ctx.rule.kind),
    )

_fuchsia_collect_unstripped_binaries_aspect = aspect(
    doc = """Collect FuchsiaUnstrippedBinaryInfo values across a DAG of dependencies,
        and provide a corresponding FuchsiaCollectedUnstrippedBinariesInfo value.""",
    implementation = _fuchsia_collect_unstripped_binaries_aspect_impl,
    attr_aspects = ["*"],
    provides = [FuchsiaCollectedUnstrippedBinariesInfo],
)

def _find_and_process_unstripped_binaries_impl(ctx):
    collected_infos = _collect_unstripped_binaries_info(ctx.attr.deps)

    prebuilt_resources = []
    generated_resources = []
    debug_infos = []

    for source_dir, unstripped_depset in collected_infos.source_dir_to_unstripped_binary.items():
        resources_to_strip = []

        for unstripped in unstripped_depset.to_list():
            if unstripped.stripped_file != None:
                prebuilt_resources.append(
                    make_resource_struct(dest = unstripped.dest, src = unstripped.stripped_file),
                )
            else:
                resources_to_strip.append(
                    make_resource_struct(dest = unstripped.dest, src = unstripped.unstripped_file),
                )

        if not resources_to_strip:
            continue

        stripped_resources, debug_info = strip_resources(ctx, resources_to_strip, source_dir = source_dir)
        generated_resources.extend(stripped_resources)
        debug_infos.append(debug_info)

    result = [
        DefaultInfo(files = depset([r.src for r in generated_resources])),
        FuchsiaPackageResourcesInfo(resources = prebuilt_resources + generated_resources),
        collect_debug_symbols(debug_infos),
    ]
    return result

find_and_process_unstripped_binaries = rule(
    doc = """Find all fuchsia_unstripped_binary() targets from a DAG of dependencies.

        Then generate actions to strip those that need it, plus another action to
        generate a .build-id/  directory populated with symlinks to the original
        unstripped files.

        Returns a FuchsiaPackageResourcesInfo provider to list all stripped binaries
        and their installation path (as used by fuchsia_package()).

        Returns a FuchsiaDebugSymbolInfo provider to list the .build-id directory
        and the corresponding source directory.
        """,
    implementation = _find_and_process_unstripped_binaries_impl,
    toolchains = ["@bazel_tools//tools/cpp:toolchain_type"],
    provides = [DefaultInfo, FuchsiaPackageResourcesInfo, FuchsiaDebugSymbolInfo],
    attrs = {
        "deps": attr.label_list(
            doc = "A list of roots for the DAG of dependencies to scan.",
            mandatory = True,
            aspects = [_fuchsia_collect_unstripped_binaries_aspect],
        ),
    } | FUCHSIA_DEBUG_SYMBOLS_ATTRS,
)
