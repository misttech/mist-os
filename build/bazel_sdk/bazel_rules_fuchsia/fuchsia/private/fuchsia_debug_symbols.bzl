# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utilities for extracting, creating, and manipulating debug symbols."""

load(":providers.bzl", "FuchsiaDebugSymbolInfo")
load(":utils.bzl", "flatten", "make_resource_struct")
load("@rules_cc//cc:find_cc_toolchain.bzl", "find_cc_toolchain")

def strip_resources(ctx, resources):
    """Strips resources and returns FuchsiaDebugSymbolInfo."""
    build_id_dir = ctx.actions.declare_directory(ctx.label.name + "/.build-id")
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

    return stripped_resources, FuchsiaDebugSymbolInfo(build_id_dirs = {
        "BUILD_WORKSPACE_DIRECTORY": depset([build_id_dir]),
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
