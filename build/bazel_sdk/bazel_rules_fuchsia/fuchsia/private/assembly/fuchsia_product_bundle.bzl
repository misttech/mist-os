# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rule for creating product bundle for flashing Fuchsia images to target devices."""

load(
    ":providers.bzl",
    "FuchsiaAssemblyConfigInfo",
    "FuchsiaProductBundleInfo",
    "FuchsiaProductImageInfo",
    "FuchsiaRepositoryKeysInfo",
    "FuchsiaScrutinyConfigInfo",
    "FuchsiaVirtualDeviceInfo",
)
load("//fuchsia/private:ffx_tool.bzl", "get_ffx_product_bundle_inputs")

def _scrutiny_validation(
        ctx,
        ffx_tool,
        pb_out_dir,
        scrutiny_config,
        is_recovery = False):
    deps = []
    ffx_invocation = [
        ffx_tool.path,
        "--isolate-dir $FFX_ISOLATE_DIR",
        "scrutiny",
        "verify",
    ]
    if is_recovery:
        ffx_invocation.append("--recovery")
    label_name = ctx.label.name + "_recovery" if is_recovery else ""
    deps.append(_verify_bootfs_filelist(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        scrutiny_config.bootfs_files,
        scrutiny_config.bootfs_packages,
    ))
    deps.append(_verify_kernel_cmdline(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        scrutiny_config.kernel_cmdline,
    ))
    deps += _verify_base_packages(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        scrutiny_config.base_packages,
    )
    if not is_recovery:
        deps += _verify_route_sources(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            scrutiny_config.routes_config_golden,
        )
        deps += _verify_component_resolver_allowlist(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            scrutiny_config.component_resolver_allowlist,
        )
        deps += _verify_routes(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            scrutiny_config.component_route_exceptions,
            scrutiny_config.component_tree_config,
        )
        deps += _verify_structured_config(
            ctx,
            ffx_invocation,
            ffx_tool,
            pb_out_dir,
            scrutiny_config.structured_config_policy,
        )
        deps += _extract_structured_config(
            ctx,
            ffx_tool,
            pb_out_dir,
            is_recovery,
        )
    return deps

def _verify_bootfs_filelist(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        zbi_bootfs_filelist_goldens,
        zbi_bootfs_packages_goldens):
    stamp_file = ctx.actions.declare_file(label_name + "_bootfs.stamp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_bootfs.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "bootfs",
        "--product-bundle",
        pb_out_dir.path,
    ]
    for golden_file in zbi_bootfs_filelist_goldens:
        _ffx_invocation += [
            "--golden",
            golden_file.path,
        ]
    for golden_package in zbi_bootfs_packages_goldens:
        _ffx_invocation += [
            "--golden-packages",
            golden_package.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir] + zbi_bootfs_filelist_goldens + zbi_bootfs_packages_goldens
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Bootfs file list for %s" % label_name,
    )
    return stamp_file

def _verify_kernel_cmdline(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        zbi_kernel_cmdline_goldens):
    stamp_file = ctx.actions.declare_file(label_name + "_kernel.stamp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_kernel.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "kernel-cmdline",
        "--product-bundle",
        pb_out_dir.path,
    ]
    for golden_file in zbi_kernel_cmdline_goldens:
        _ffx_invocation += [
            "--golden",
            golden_file.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir] + zbi_kernel_cmdline_goldens
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Kernel Cmdline for %s" % label_name,
    )
    return stamp_file

def _verify_route_sources(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        routes_config_golden):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_route.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_route.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_route.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "route-sources",
        "--product-bundle",
        pb_out_dir.path,
        "--config",
        routes_config_golden.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, routes_config_golden]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Route sources for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _verify_component_resolver_allowlist(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        component_resolver_allowlist):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_component_resolver.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_component_resolver.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_component_resolver.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "component-resolvers",
        "--product-bundle",
        pb_out_dir.path,
        "--allowlist",
        component_resolver_allowlist.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, component_resolver_allowlist]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Component Resolver for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _verify_routes(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        allow_lists,
        component_tree_config):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_routes.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_routes.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_routes.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "routes",
        "--product-bundle",
        pb_out_dir.path,
    ]
    if component_tree_config:
        _ffx_invocation += [
            "--component-tree-config",
            component_tree_config.path,
        ]
    for allow_list in allow_lists:
        _ffx_invocation += [
            "--allowlist",
            allow_list.path,
        ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, component_tree_config] + allow_lists
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Routes for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _verify_base_packages(
        ctx,
        label_name,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        base_packages):
    stamp_file = ctx.actions.declare_file(label_name + "_static_pkgs.stamp")
    tmp_dir = ctx.actions.declare_directory(label_name + "_static_pkgs.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(label_name + "_static_pkgs.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "static-pkgs",
        "--product-bundle",
        pb_out_dir.path,
        "--golden",
        base_packages.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, base_packages]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify Static pkgs for %s" % label_name,
    )
    return [stamp_file, tmp_dir]

def _verify_structured_config(
        ctx,
        ffx_invocation,
        ffx_tool,
        pb_out_dir,
        structured_config_policy):
    stamp_file = ctx.actions.declare_file(ctx.label.name + "_structured_config.stamp")
    tmp_dir = ctx.actions.declare_directory(ctx.label.name + "_structured_config.tmp")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_structured_config.ffx")
    _ffx_invocation = []
    _ffx_invocation.extend(ffx_invocation)
    _ffx_invocation += [
        "--stamp",
        stamp_file.path,
        "--tmp-dir",
        tmp_dir.path,
        "structured-config",
        "--product-bundle",
        pb_out_dir.path,
        "--policy",
        structured_config_policy.path,
    ]
    script_lines = [
        "set -e",
        " ".join(_ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir, structured_config_policy]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [stamp_file, tmp_dir, ffx_isolate_dir],
        env = {"FFX_ISOLATE_DIR": ffx_isolate_dir.path},
        command = "\n".join(script_lines),
        progress_message = "Verify structured config for %s" % ctx.label.name,
    )
    return [stamp_file, tmp_dir]

def _extract_structured_config(ctx, ffx_tool, pb_out_dir, is_recovery):
    structured_config = ctx.actions.declare_file(ctx.label.name + "_structured_config")
    depfile = ctx.actions.declare_file(ctx.label.name + "_depfile")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_extract_structured_config.ffx")
    ffx_invocation = [
        ffx_tool.path,
        "--isolate-dir " + ffx_isolate_dir.path,
        "scrutiny",
        "extract",
        "structured-config",
        "--product-bundle",
        pb_out_dir.path,
        "--output",
        structured_config.path,
        # These args are currently required, but the outputs are ignored.
        "--build-path",
        pb_out_dir.path,
        "--depfile",
        depfile.path,
    ]
    if is_recovery:
        ffx_invocation.append("--recovery")
    script_lines = [
        "set -e",
        " ".join(ffx_invocation),
    ]
    inputs = [ffx_tool, pb_out_dir]
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [structured_config, depfile, ffx_isolate_dir],
        command = "\n".join(script_lines),
        progress_message = "Extract structured config for %s" % ctx.label.name,
    )
    return [structured_config, depfile]

def _fuchsia_product_bundle_impl(ctx):
    fuchsia_toolchain = ctx.toolchains["@fuchsia_sdk//fuchsia:toolchain"]
    partitions_configuration = ctx.attr.partitions_config[FuchsiaAssemblyConfigInfo].config
    system_a_out = ctx.attr.product_image[FuchsiaProductImageInfo].images_out
    ffx_tool = fuchsia_toolchain.ffx
    pb_out_dir = ctx.actions.declare_directory(ctx.label.name + "_out")
    ffx_isolate_dir = ctx.actions.declare_directory(ctx.label.name + "_ffx_isolate_dir")
    product_name = "{}.{}".format(ctx.attr.product_name, ctx.attr.board_name)

    # In the future, the product bundles should be versioned independently of
    # the sdk version. So far they have been the same value.
    product_version = ctx.attr.product_version or fuchsia_toolchain.sdk_id
    if not product_version:
        fail("product_version string must not be empty.")

    # Gather all the arguments to pass to ffx.
    ffx_invocation = [
        "$FFX",
        "--config \"product.experimental=true,sdk.root=$SDK_ROOT\"",
        "--isolate-dir $FFX_ISOLATE_DIR",
        "product",
        "create",
        "--product-name",
        product_name,
        "--product-version",
        product_version,
        "--partitions $PARTITIONS_PATH",
        "--system-a $SYSTEM_A_MANIFEST",
        "--out-dir $OUTDIR",
    ]

    # Gather the environment variables needed in the script.
    env = {
        "FFX": ffx_tool.path,
        "OUTDIR": pb_out_dir.path,
        "PARTITIONS_PATH": partitions_configuration.path,
        "SYSTEM_A_MANIFEST": system_a_out.path + "/images.json",
        "FFX_ISOLATE_DIR": ffx_isolate_dir.path,
        "SDK_ROOT": ctx.attr._sdk_manifest.label.workspace_root,
    }

    # Gather all the inputs.
    inputs = ctx.files.partitions_config + ctx.files.product_image + get_ffx_product_bundle_inputs(fuchsia_toolchain)

    # Add virtual devices.
    for virtual_device in ctx.attr.virtual_devices:
        ffx_invocation.append("--virtual-device")
        ffx_invocation.append(virtual_device[FuchsiaVirtualDeviceInfo].config.path)
        inputs.append(virtual_device[FuchsiaVirtualDeviceInfo].config)
        inputs.append(virtual_device[FuchsiaVirtualDeviceInfo].template)
    if ctx.attr.default_virtual_device != None:
        ffx_invocation.append("--recommended-device")
        ffx_invocation.append(ctx.attr.default_virtual_device[FuchsiaVirtualDeviceInfo].device_name)

    # If recovery is supplied, add it to the product bundle.
    if ctx.attr.recovery != None:
        system_r_out = ctx.attr.recovery[FuchsiaProductImageInfo].images_out
        ffx_invocation.append("--system-r $SYSTEM_R_MANIFEST")
        env["SYSTEM_R_MANIFEST"] = system_r_out.path + "/images.json"
        inputs.extend(ctx.files.recovery)

    # If update info is supplied, add it to the product bundle.
    if ctx.file.update_version_file != None:
        if ctx.attr.repository_keys == None:
            fail("Repository keys must be supplied in order to build an update package")
        ffx_invocation.extend([
            "--update-package-version-file $UPDATE_VERSION_FILE",
            "--update-package-epoch $UPDATE_EPOCH",
        ])
        env["UPDATE_VERSION_FILE"] = ctx.file.update_version_file.path
        env["UPDATE_EPOCH"] = ctx.attr.update_epoch
        inputs.append(ctx.file.update_version_file)

    # If a repository is supplied, add it to the product bundle.
    if ctx.attr.repository_keys != None:
        ffx_invocation.append("--tuf-keys $REPOKEYS")
        env["REPOKEYS"] = ctx.attr.repository_keys[FuchsiaRepositoryKeysInfo].dir
        inputs += ctx.files.repository_keys
    script_lines = [
        "set -e",
        "mkdir -p $FFX_ISOLATE_DIR",
        " ".join(ffx_invocation),
    ]

    script_lines.append("cp $BOOTSERVER $OUTDIR")
    env["BOOTSERVER"] = fuchsia_toolchain.bootserver.path
    inputs.append(fuchsia_toolchain.bootserver)

    script = "\n".join(script_lines)
    ctx.actions.run_shell(
        inputs = inputs,
        outputs = [pb_out_dir, ffx_isolate_dir],
        command = script,
        env = env,
        progress_message = "Creating product bundle for %s" % ctx.label.name,
    )
    deps = [pb_out_dir] + ctx.files.partitions_config + ctx.files.product_image

    # Scrutiny Validation
    if ctx.attr.product_image_scrutiny_config:
        product_image_scrutiny_config = ctx.attr.product_image_scrutiny_config[FuchsiaScrutinyConfigInfo]
        deps += _scrutiny_validation(ctx, ffx_tool, pb_out_dir, product_image_scrutiny_config)
    if ctx.attr.recovery_scrutiny_config:
        recovery_scrutiny_config = ctx.attr.recovery_scrutiny_config[FuchsiaScrutinyConfigInfo]
        deps += _scrutiny_validation(ctx, ffx_tool, pb_out_dir, recovery_scrutiny_config, True)

    # TODO(fxb/121752): Remove the generation of pave.sh after infra is
    # ready to use product bundle to flash device.
    pave_script = ctx.actions.declare_file("pave.sh")
    ctx.actions.run(
        outputs = [pave_script],
        inputs = [pb_out_dir],
        executable = ctx.executable._create_pave_script,
        arguments = [
            "--product-bundle",
            pb_out_dir.path + "/product_bundle.json",
            "--pave-script-path",
            pave_script.path,
        ],
    )
    deps.append(pave_script)

    return [DefaultInfo(files = depset(direct = deps)), FuchsiaProductBundleInfo(
        product_bundle = pb_out_dir,
        is_remote = False,
    )]

fuchsia_product_bundle = rule(
    doc = """Creates pb for flashing Fuchsia images to target devices.""",
    implementation = _fuchsia_product_bundle_impl,
    toolchains = ["@fuchsia_sdk//fuchsia:toolchain"],
    attrs = {
        "board_name": attr.string(
            doc = "Name of the board this PB runs on. E.g. qemu-x64",
            mandatory = True,
        ),
        "partitions_config": attr.label(
            doc = "Partitions config to use",
            mandatory = True,
        ),
        "product_image": attr.label(
            doc = "fuchsia_product_image target to put in slot A",
            providers = [FuchsiaProductImageInfo],
            mandatory = True,
        ),
        "product_name": attr.string(
            doc = "Name of the Fuchsia product. E.g. workstation_eng",
            mandatory = True,
        ),
        "product_version": attr.string(
            doc = "Version of the Fuchsia product. E.g. 35.20221231.0.1",
        ),
        "recovery": attr.label(
            doc = "fuchsia_product_image target to put in slot R",
            providers = [FuchsiaProductImageInfo],
        ),
        "repository_keys": attr.label(
            doc = "A fuchsia_repository_keys target, must be specified when update_version_file is specified",
            providers = [FuchsiaRepositoryKeysInfo],
            default = None,
        ),
        "update_version_file": attr.label(
            doc = "version file needed to create update package",
            allow_single_file = True,
            default = None,
        ),
        "update_epoch": attr.string(
            doc = "epoch needed to create update package",
            default = "1",
        ),
        "product_image_scrutiny_config": attr.label(
            doc = "Scrutiny config for slot A",
            providers = [FuchsiaScrutinyConfigInfo],
        ),
        "recovery_scrutiny_config": attr.label(
            doc = "Scrutiny config for recovery",
            providers = [FuchsiaScrutinyConfigInfo],
        ),
        "virtual_devices": attr.label_list(
            doc = "Virtual devices to make available",
            providers = [FuchsiaVirtualDeviceInfo],
            default = [],
        ),
        "default_virtual_device": attr.label(
            doc = "Default virtual device to run when none is specified",
            providers = [FuchsiaVirtualDeviceInfo],
            default = None,
        ),
        "_sdk_manifest": attr.label(
            allow_single_file = True,
            default = "@fuchsia_sdk//:meta/manifest.json",
        ),
        "_create_pave_script": attr.label(
            default = "//fuchsia/tools:create_pave_script",
            executable = True,
            cfg = "exec",
        ),
    },
)
