# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Rule for creating virtual devices for running in an emulator."""

load(":providers.bzl", "FuchsiaVirtualDeviceInfo")

ARCH = struct(
    X64 = "x64",
    ARM64 = "arm64",
)

def _fuchsia_virtual_device_impl(ctx):
    # Copy the start up arguments template next to the output virtual device.
    template_file_name = ctx.attr.device_name + ".json.template"
    template_file = ctx.actions.declare_file(template_file_name)
    ctx.actions.run_shell(
        inputs = [ctx.file._start_up_args_template],
        outputs = [template_file],
        command = "cp $1 $2",
        arguments = [ctx.file._start_up_args_template.path, template_file.path],
    )

    virtual_device_file = ctx.actions.declare_file(ctx.attr.device_name + ".json")
    virtual_device = {
        "schema_id": "http://fuchsia.com/schemas/sdk/virtual_device-93A41932.json",
        "data": {
            "type": "virtual_device",
            "name": ctx.attr.device_name,
            "description": ctx.attr.description,
            "hardware": {
                "cpu": {
                    "arch": ctx.attr.arch,
                },
                "audio": {
                    "model": "hda",
                },
                "inputs": {
                    # Touch is the default to avoid issues with mouse capture
                    # especially with cloudtops.
                    "pointing_device": "touch",
                },
                "window_size": {
                    "height": 800,
                    "width": 1280,
                    "units": "pixels",
                },
                "memory": {
                    "quantity": 8192,
                    "units": "megabytes",
                },
                "storage": {
                    "quantity": 2,
                    "units": "gigabytes",
                },
            },
            "ports": {
                "ssh": 22,
                "mdns": 5353,
                "debug": 2345,
            },

            # TODO(fxbug.dev/94125): remove once solution is available.
            "start_up_args_template": template_file_name,
        },
    }
    ctx.actions.write(virtual_device_file, json.encode(virtual_device))

    return [
        FuchsiaVirtualDeviceInfo(
            device_name = ctx.attr.device_name,
            config = virtual_device_file,
            template = template_file,
        ),
    ]

fuchsia_virtual_device = rule(
    doc = """Creates a fuchsia virtual device for running in an emulator.""",
    implementation = _fuchsia_virtual_device_impl,
    attrs = {
        "device_name": attr.string(
            doc = "Name of the virtual device",
            mandatory = True,
        ),
        "description": attr.string(
            doc = "Description of the virtual device",
            default = "",
        ),
        "arch": attr.string(
            doc = "The architecture of the cpu",
            values = [ARCH.X64, ARCH.ARM64],
            mandatory = True,
        ),
        "_start_up_args_template": attr.label(
            allow_single_file = True,
            default = "@rules_fuchsia//fuchsia/private:templates/emulator_flags.json.template",
        ),
    },
)
