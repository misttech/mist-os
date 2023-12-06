# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Convenience function to generate a JSON validating script."""

load("//test_utils:py_test_utils.bzl", "PY_TOOLCHAIN_DEPS")

# Script template
_validator_command_template = """\
#!/bin/sh

python3 {validator} \
    --generated {generated} \
    --golden "{golden}" \
    "$@"
"""

# These attributes must be part of any rule() whose implementation
# function wants to call create_validation_script() below. They
# are used to ensure to ensure the function can access the
# paths and runfiles of json_comparator.py and of the python
# interpreter.
CREATE_VALIDATION_SCRIPT_ATTRS = {
    "_json_comparator": attr.label(
        default = "@fuchsia_sdk//fuchsia/tools:json_comparator",
        executable = True,
        cfg = "exec",
    ),
} | PY_TOOLCHAIN_DEPS

def create_validation_script(ctx, generated_file, golden_file):
    """Create a validation script and its related runfiles object.

    Create a validation script that invokes json_comparator.py to
    verify that a given generated file matches a golden file.

    Args:
      ctx: A rule context object. The corresponding rule definition
         *must* include the content of CREATE_VALIDATION_SCRIPT_ATTRS
         in its `attrs` value.

      generated_file: a File object pointing to the generated
         file to verify.

      golden_file: a File object pointing to the golden file used
         for verification.

    Returns:
      A (script, runfiles) pair, where `script` is an output File
      corresponding to the comparison script produced by the
      action produced by this function, and `runfiles` corresponds
      to the files needed to run it at runtime.
    """
    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    script_content = _validator_command_template.format(
        validator = ctx.executable._json_comparator.short_path,
        generated = generated_file.short_path,
        golden = golden_file.short_path,
    )
    ctx.actions.write(script, script_content, is_executable = True)

    runfiles = ctx.runfiles(
        files = [
            golden_file,
            generated_file,
        ],
    ).merge_all([
        ctx.attr._json_comparator[DefaultInfo].default_runfiles,
        ctx.attr._py_toolchain[DefaultInfo].default_runfiles,
    ])

    return script, runfiles
