# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

load("//fuchsia/private:providers.bzl", "FuchsiaPackageInfo")
load(":providers.bzl", "FuchsiaAssembledPackageInfo", "FuchsiaConfigDataInfo")

def _fuchsia_assemble_package_impl(ctx):
    configs = []
    files = []
    for config_file in ctx.attr.configs:
        f = config_file.files.to_list()[0]
        configs.append(FuchsiaConfigDataInfo(
            source = f,
            destination = ctx.attr.configs[config_file],
        ))
        files.append(f)
    package = ctx.attr.package[FuchsiaPackageInfo]
    files.extend(ctx.files.package)

    return [
        DefaultInfo(files = depset(files)),
        FuchsiaAssembledPackageInfo(
            package = package,
            configs = configs,
            files = files,
        ),
    ]

fuchsia_assemble_package = rule(
    doc = """Generates assembled config based on package and their config.""",
    implementation = _fuchsia_assemble_package_impl,
    provides = [FuchsiaAssembledPackageInfo],
    attrs = {
        "package": attr.label(
            providers = [FuchsiaPackageInfo],
            mandatory = True,
        ),
        "configs": attr.label_keyed_string_dict(
            doc = "Config-datas that are attached to the package. It's a dictionary of source files to destination string",
            allow_files = True,
        ),
    },
)
