# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating a ELF sizes summary file for a Fuchsia image."""

load("//fuchsia/constraints:target_compatibility.bzl", "COMPATIBILITY")
load("//fuchsia/private:fuchsia_toolchains.bzl", "FUCHSIA_TOOLCHAIN_DEFINITION", "get_fuchsia_sdk_toolchain")
load(":providers.bzl", "FuchsiaProductImageInfo")
load(":utils.bzl", "LOCAL_ONLY_ACTION_KWARGS")

def _fuchsia_elf_sizes_impl(ctx):
    images_out = ctx.attr.product[FuchsiaProductImageInfo].images_out
    zbi = get_fuchsia_sdk_toolchain(ctx).zbi

    gen_dir_path = "{basedir}/{label_name}_gen".format(
        basedir = images_out.dirname,
        label_name = ctx.label.name,
    )
    elf_sizes_json = ctx.actions.declare_file(ctx.label.name + "_elf_sizes.json")

    ctx.actions.run(
        inputs = ctx.files.product + [zbi],
        outputs = [
            elf_sizes_json,
        ],
        executable = ctx.executable._elf_sizes_py,
        arguments = [
            "--assembly-dir",
            images_out.path,
            "--zbi-tool",
            zbi.path,
            "--scratch-dir",
            gen_dir_path,
            "--sizes",
            elf_sizes_json.path,
        ],
        mnemonic = "ElfSizes",
        **LOCAL_ONLY_ACTION_KWARGS
    )

    return [
        DefaultInfo(files = depset(direct = [elf_sizes_json])),
    ]

fuchsia_elf_sizes = rule(
    doc = """Create a ELF sizes summary file for a Fuchsia product.""",
    implementation = _fuchsia_elf_sizes_impl,
    toolchains = [FUCHSIA_TOOLCHAIN_DEFINITION],
    attrs = {
        "product": attr.label(
            doc = "The fuchsia product to check the size of.",
            providers = [FuchsiaProductImageInfo],
            mandatory = True,
        ),
        "_elf_sizes_py": attr.label(
            default = "//fuchsia/tools:elf_sizes",
            executable = True,
            cfg = "exec",
        ),
    } | COMPATIBILITY.HOST_ATTRS,
)
