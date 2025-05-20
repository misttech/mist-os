# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities for extracting, creating, and manipulating debug symbols."""

load(
    ":providers.bzl",
    "FuchsiaCollectedUnstrippedBinariesInfo",
    "FuchsiaDebugSymbolInfo",
    "FuchsiaPackageResourcesInfo",
    "FuchsiaUnstrippedBinaryInfo",
)
load(":utils.bzl", "find_cc_toolchain", "flatten", "get_target_deps_from_attributes", "make_resource_struct")

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

def strip_resources(ctx, resources, build_id_path = None, source_search_root = "BUILD_WORKSPACE_DIRECTORY"):
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
      source_search_root: (optional) Either a string or File value, see
        FuchsiaDebugSymbolInfo documentation.

    Returns:
      a pair whose first item is a list of stripped resource_struct() instances,
      and the second item is a FuchsiaDebugSymbolInfo provider for the
      corresponding .build-id directory that contains a single
      (source_search_root, build_dirs_depset) pair.
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
    # once so there is no need to attempt to strip duplicates.
    for r in depset(resources).to_list():
        ids_txt = ctx.actions.declare_file(r.src.path + ".ids_txt")
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

    if type(source_search_root) not in ("File", "string"):
        fail("The 'source_search_root' argument should be a string or a File value, got: %s" % repr(source_search_root))

    return stripped_resources, FuchsiaDebugSymbolInfo(build_id_dirs_mapping = {
        source_search_root: depset([build_id_dir]),
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
        FuchsiaDebugSymbolInfo(build_id_dirs_mapping = {
            ctx.file.source_search_root: depset(transitive = [
                target[DefaultInfo].files
                for target in ctx.attr.build_id_dirs
            ]),
        }),
    ]

fuchsia_debug_symbols = rule(
    doc = """Rule-based constructor for FuchsiaDebugSymbolInfo.""",
    implementation = _fuchsia_debug_symbols_impl,
    attrs = {
        "source_search_root": attr.label(
            doc = "A search root file or directory, used by zxdb to locate source files.",
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

def merge_debug_symbol_infos(*targets_or_providers):
    """Merge the FuchsiaDebugSymbolInfo values from a list of targets or providers.

    Args:
        targets_or_providers: a list whose flattened elements must be provider
           or target values.
    Returns:
        A new FuchsiaSymbolDebugSymbolInfo resulting from merging the input
        FuchsiaDebugSymbolInfo values together.
    """

    # { source_search_root -> list[depset[build_id_dir]]}
    source_search_root_map = {}

    for target_or_provider in flatten(targets_or_providers):
        if hasattr(target_or_provider, "build_id_dirs_mapping"):
            build_id_dirs_map = target_or_provider.build_id_dirs_mapping
        elif FuchsiaDebugSymbolInfo in target_or_provider:
            build_id_dirs_map = target_or_provider[FuchsiaDebugSymbolInfo].build_id_dirs_mapping
        else:
            continue

        for source_search_root, build_id_dirs_depset in build_id_dirs_map.items():
            if source_search_root not in source_search_root_map:
                source_search_root_map[source_search_root] = []
            source_search_root_map[source_search_root].append(build_id_dirs_depset)

    return FuchsiaDebugSymbolInfo(
        build_id_dirs_mapping = {
            source_search_root: depset(transitive = build_id_dirs_depsets)
            for source_search_root, build_id_dirs_depsets in source_search_root_map.items()
        },
    )

def _fuchsia_unstripped_binary_impl(ctx):
    return FuchsiaUnstrippedBinaryInfo(
        dest = ctx.attr.dest,
        unstripped_file = ctx.file.unstripped_file,
        stripped_file = ctx.file.stripped_file if ctx.attr.stripped_file else None,
        source_search_root = ctx.attr.source_search_root,
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
        "source_search_root": attr.label(
            doc = "Optional label to source directory or file inside source directory.",
            mandatory = False,
            allow_single_file = True,
        ),
    },
)

def _convert_unstripped_binary_info(binary_info):
    """Convert a FuchsiaUnstrippedBinaryInfo into an equivalent FuchsiaCollectedUnstrippedBinariesInfo value.

    Args:
        binary_info: A FuchsiaUnstrippedBinaryInfo value,
    Returns:
        A corresponding FuchsiaCollectedUnstrippedBinariesInfo.
    """
    source_search_root = binary_info.source_search_root
    if source_search_root == None:
        source_search_root = "BUILD_WORKSPACE_DIRECTORY"
    return FuchsiaCollectedUnstrippedBinariesInfo(
        source_search_root_to_unstripped_binary = {
            source_search_root: depset([
                struct(
                    dest = binary_info.dest,
                    unstripped_file = binary_info.unstripped_file,
                    stripped_file = binary_info.stripped_file,
                ),
            ]),
        },
    )

def _merge_unstripped_binaries_infos(*targets_or_providers):
    """Merge any number of unstripped binary info providers or targets.

    Args:
       *targets_or_providers: the list of arguments will be flattened, and items that
            are FuchsiaCollectedUnstrippedBinariesInfo providers, or targets that provide
            such values will be used as direct inputs for the merge. Items that
            are FuchsiaUnstrippedBinaryInfo or targets that provide such values will
            first be converted into a FuchsiaCollectedUnstrippedBinariesInfo value and
            the result will be used as input for the merge.
    Returns:
        A new FuchsiaCollectedUnstrippedBinariesInfo value, merging the content of
        the input arguments.
    """

    # Map { source_search_root -> list[depset[struct(dest, unstripped_file, stripped_file)]] }
    # will be turned into a source_search_root_to_unstripped_binary dict.
    source_search_root_map = {}

    # list[depset[File]] for unstripped_files
    unstripped_files_depsets = []

    for t in flatten(targets_or_providers):
        if hasattr(t, "source_search_root_to_unstripped_binary"):
            collected_info = t
        elif hasattr(t, "unstripped_binary") and hasattr(t, "dest"):
            collected_info = _convert_unstripped_binary_info(t)
        elif type(t) == "Target":
            if FuchsiaCollectedUnstrippedBinariesInfo in t:
                collected_info = t[FuchsiaCollectedUnstrippedBinariesInfo]
            elif FuchsiaUnstrippedBinaryInfo in t:
                collected_info = _convert_unstripped_binary_info(t[FuchsiaUnstrippedBinaryInfo])
            else:
                continue
        else:
            fail("Invalid type {} of provider/target value {}".format(type(t), repr(t)))

        for source_search_root, binary_info_depset in collected_info.source_search_root_to_unstripped_binary.items():
            if source_search_root not in source_search_root_map:
                source_search_root_map[source_search_root] = []
            source_search_root_map[source_search_root].append(binary_info_depset)

    return FuchsiaCollectedUnstrippedBinariesInfo(
        source_search_root_to_unstripped_binary = {
            source_search_root: depset(transitive = binary_info_depsets)
            for source_search_root, binary_info_depsets in source_search_root_map.items()
        },
    )

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
    return get_target_deps_from_attributes(rule_attr, rule_kind, known_rule_kinds = _KNOWN_RULE_KINDS_TO_DEP_ATTR_NAMES)

def _fuchsia_collect_unstripped_binaries_aspect_impl(target, aspect_ctx):
    return _merge_unstripped_binaries_infos(
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
    all_collected_unstripped_binaries_info = _merge_unstripped_binaries_infos(ctx.attr.deps)

    prebuilt_resources = []

    # list[resource_struct]
    generated_resources = []

    # list[FuchsiaDebugSymbolInfo] covering the symbols of all stripped binaries.
    stripped_debug_symbol_infos = []

    for source_search_root, unstripped_depset in all_collected_unstripped_binaries_info.source_search_root_to_unstripped_binary.items():
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

        stripped_resources, debug_symbol_info = strip_resources(ctx, resources_to_strip, source_search_root = source_search_root)
        generated_resources.extend(stripped_resources)
        stripped_debug_symbol_infos.append(debug_symbol_info)

    outputs = depset(
        direct = [r.src for r in generated_resources],
        # strip_resources creates a FuchsiaDebugSymbol mapping with a single (key, value) pair.
        # the value is a depset() covering the generated .build-id directories.
        transitive = [debug_symbol_info.build_id_dirs_mapping.values()[0] for debug_symbol_info in stripped_debug_symbol_infos],
    )

    result = [
        DefaultInfo(files = outputs),
        FuchsiaPackageResourcesInfo(resources = prebuilt_resources + generated_resources),
        merge_debug_symbol_infos(stripped_debug_symbol_infos),
        all_collected_unstripped_binaries_info,  # A FuchsiaCollectedUnstrippedBinaryInfo value.
    ]
    return result

find_and_process_unstripped_binaries = rule(
    doc = """Find all fuchsia_unstripped_binary() targets from a DAG of dependencies.

        Then generate actions to strip those that need it, plus other actions to
        generate a .build-id/  directory populated with symlinks to the original
        unstripped files.

        Returns a FuchsiaPackageResourcesInfo provider to list all stripped binaries
        and their installation path (as used by fuchsia_package()).

        Returns a FuchsiaDebugSymbolInfo provider to list the .build-id directories
        and the corresponding source search roots.

        Returns a FuchsiaCollectedUnstrippedBinariesInfo provider to list all the
        collected files.
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

_FuchsiaCollectedDebugSymbolsInfo = provider(
    "Contains a collection of debug symbols that were collected through an aspect",
    fields = {
        "collected_symbols": "A depset containing the direct and transitive symbols",
    },
)

def transform_collected_debug_symbols_infos(*targets):
    """ Transforms a list targets which into a FuchsiaDebugSymbolsInfo

    Given a list of targets which have had the fuchsia_collect_all_debug_symbols_infos_aspect
    run against them, collect all the debug symbols into a single FuchsiaDebugSymbolsInfo.

    Args:
      *targets: A list of targets. It is ok to pass a list that contains None values

    Returns:
      A FuchsiaDebugSymbolsInfo provider.
    """
    valid_targets = []
    for target_or_list in targets:
        for t in (target_or_list if type(target_or_list) == "list" else [target_or_list]):
            if t and _FuchsiaCollectedDebugSymbolsInfo in t:
                valid_targets.append(t)

    return merge_debug_symbol_infos(
        flatten([
            t[_FuchsiaCollectedDebugSymbolsInfo].collected_symbols.to_list()
            for t in valid_targets
        ]),
    )

def _fuchsia_collect_all_debug_symbols_infos_aspect_impl(target, ctx):
    return _FuchsiaCollectedDebugSymbolsInfo(
        collected_symbols = depset(
            direct = [target[FuchsiaDebugSymbolInfo]] if FuchsiaDebugSymbolInfo in target else [],
            transitive = [t[_FuchsiaCollectedDebugSymbolsInfo].collected_symbols for t in get_target_deps_from_attributes(
                ctx.rule.attr,
                ctx.rule.kind,
            ) if _FuchsiaCollectedDebugSymbolsInfo in t],
        ),
    )

fuchsia_collect_all_debug_symbols_infos_aspect = aspect(
    doc = """Collects all of the FuchsiaDebugSymbolInfo providers in the graph.

    This aspect will walk the dependency tree finding all of the targets that
    expose the FuchsiaDebugSymbolInfo provider and collect them into a top top
    level provider.

    To convert the collected resources back into a FuchsiaDebugSymbolInfo object
    call the transform_collected_debug_symbols_infos method with the teop
    """,
    implementation = _fuchsia_collect_all_debug_symbols_infos_aspect_impl,
    attr_aspects = ["*"],
)
