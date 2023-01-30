# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for defining a Fuchsia toolchain."""

load("//fuchsia/private:providers.bzl", "FuchsiaComponentManifestShardCollectionInfo")

def _fuchsia_toolchain_info_impl(ctx):
    return [platform_common.ToolchainInfo(
        name = ctx.label.name,
        bootserver = ctx.executable.bootserver,
        blobfs = ctx.executable.blobfs,
        blobfs_manifest = ctx.file.blobfs_manifest,
        bindc = ctx.executable.bindc or None,
        cmc = ctx.executable.cmc,
        cmc_manifest = ctx.file.cmc_manifest,
        cmc_includes = ctx.attr.cmc_includes or None,
        far = ctx.executable.far,
        ffx = ctx.executable.ffx,
        fssh = ctx.executable.fssh,
        fidlc = ctx.executable.fidlc,
        fidlgen_hlcpp = ctx.executable.fidlgen_hlcpp,
        fidlgen_cpp = ctx.executable.fidlgen_cpp,
        fvm = ctx.executable.fvm,
        fvm_manifest = ctx.file.fvm_manifest,
        merkleroot = ctx.executable.merkleroot,
        minfs = ctx.executable.minfs,
        minfs_manifest = ctx.file.minfs_manifest,
        pm = ctx.executable.pm,
        zbi = ctx.executable.zbi,
        zbi_manifest = ctx.file.zbi_manifest,
        default_api_level = ctx.attr.default_target_api,
        default_fidl_target_api = ctx.attr.default_fidl_target_api,
        exec_cpu = ctx.attr.exec_cpu,
        sdk_id = ctx.attr.sdk_id,
        sdk_manifest = ctx.file.sdk_manifest,
    )]

fuchsia_toolchain_info = rule(
    implementation = _fuchsia_toolchain_info_impl,
    doc = """
Fuchsia toolchain info rule, to be passed to the native `toolchain` rule.

It provides information about tools in the Fuchsia toolchain, primarily those
included in the Fuchsia IDK.
""",
    attrs = {
        "bootserver": attr.label(
            doc = "bootserver executable",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "blobfs": attr.label(
            doc = "blobfs tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "blobfs_manifest": attr.label(
            doc = "blobfs tool's manifest, required by ffx.",
            mandatory = True,
            cfg = "exec",
            allow_single_file = True,
        ),
        "bindc": attr.label(
            doc = "bindc tool executable.",
            mandatory = False,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "cmc": attr.label(
            doc = "cmc tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "cmc_manifest": attr.label(
            doc = "cmc tool's manifest, required by ffx.",
            mandatory = True,
            cfg = "exec",
            allow_single_file = True,
        ),
        "cmc_includes": attr.label(
            doc = "The collection of cml files to include in the cmc invocation",
            providers = [[FuchsiaComponentManifestShardCollectionInfo]],
        ),
        "far": attr.label(
            doc = "far tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "ffx": attr.label(
            doc = "ffx tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "fssh": attr.label(
            doc = "fssh tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "fidlc": attr.label(
            doc = "fidlc tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "fidlgen_hlcpp": attr.label(
            doc = "fidlgen_hlcpp tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "fidlgen_cpp": attr.label(
            doc = "fidlgen_cpp tool executable.",
            mandatory = False,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "fvm": attr.label(
            doc = "fvm tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "fvm_manifest": attr.label(
            doc = "fvm tool's manifest, required by ffx.",
            mandatory = True,
            cfg = "exec",
            allow_single_file = True,
        ),
        "merkleroot": attr.label(
            doc = "merkleroot tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "minfs": attr.label(
            doc = "minfs tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "minfs_manifest": attr.label(
            doc = "minfs tool's manifest, required by ffx.",
            mandatory = True,
            cfg = "exec",
            allow_single_file = True,
        ),
        "pm": attr.label(
            doc = "pm tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "zbi": attr.label(
            doc = "zbi tool executable.",
            mandatory = True,
            cfg = "exec",
            executable = True,
            allow_single_file = True,
        ),
        "zbi_manifest": attr.label(
            doc = "zbi tool's manifest, required by ffx.",
            mandatory = True,
            cfg = "exec",
            allow_single_file = True,
        ),
        "default_target_api": attr.int(
            doc = "Default platform target api.",
            mandatory = True,
        ),
        "default_fidl_target_api": attr.string(
            doc = "Default platform target api for FIDL.",
            mandatory = True,
        ),
        "exec_cpu": attr.string(
            doc = "The exec cpu configuration.",
            mandatory = True,
            values = ["x64", "arm64"],
        ),
        "sdk_id": attr.string(
            doc = "The identifier for this sdk toolchain.",
            mandatory = True,
        ),
        "sdk_manifest": attr.label(
            doc = "Top-level SDK manifest location.",
            mandatory = True,
            allow_single_file = True,
        ),
    },
    provides = [platform_common.ToolchainInfo],
)
