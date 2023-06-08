# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating a ELF sizes summary file for a Fuchsia image."""

load(":providers.bzl", "FuchsiaProductImageInfo")

def _fuchsia_elf_sizes_impl(ctx):
    images_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out
    zbi = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"].zbi

    extracted_zbi_bootfs_dir = ctx.actions.declare_directory(ctx.label.name + "_extracted_zbi_bootfs")
    extracted_zbi_json = ctx.actions.declare_file(ctx.label.name + "_extracted_zbi_bootfs.json")
    ctx.actions.run_shell(
        inputs = [zbi] + ctx.files.product_image,
        outputs = [
            extracted_zbi_bootfs_dir,
            extracted_zbi_json,
        ],
        command = " ".join([
            zbi.path,
            "--extract",
            "--output-dir",
            extracted_zbi_bootfs_dir.path,
            "--json-output",
            extracted_zbi_json.path,
            # NOTE: This currently only supports fuchsia.zbi, for other ZBIs
            # we'll need a way to figure out their names.
            images_out.path + "/fuchsia.zbi",
        ]),
        progress_message = "Extracting bootfs for %s" % ctx.label.name,
    )

    elf_sizes_json = ctx.actions.declare_file(ctx.label.name + "_elf_sizes.json")

    ctx.actions.run(
        inputs = ctx.files.product_image + [
            extracted_zbi_bootfs_dir,
            extracted_zbi_json,
        ],
        outputs = [
            elf_sizes_json,
        ],
        executable = ctx.executable._elf_sizes_py,
        arguments = [
            "--sizes",
            elf_sizes_json.path,
            "--blobs",
            images_out.path + "/blobs.json",
            "--zbi",
            extracted_zbi_json.path,
            "--bootfs-dir",
            extracted_zbi_bootfs_dir.path,
        ],
    )

    return [
        DefaultInfo(files = depset(direct = [elf_sizes_json])),
    ]

fuchsia_elf_sizes = rule(
    doc = """Create a ELF sizes summary file for a Fuchsia image.""",
    implementation = _fuchsia_elf_sizes_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "product_image": attr.label(
            doc = "fuchsia_product_image target to check size",
            providers = [FuchsiaProductImageInfo],
            mandatory = True,
        ),
        "_elf_sizes_py": attr.label(
            default = "//fuchsia/tools:elf_sizes",
            executable = True,
            cfg = "exec",
        ),
    },
)
