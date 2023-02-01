# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

_validator_command_template = """\
#!/bin/bash

{validator} \
    --generated {generated} \
    --golden "{golden}" \
    "$@"
"""

def create_validation_script(ctx, generated_file, golden_file):
    script = ctx.actions.declare_file(ctx.label.name + ".sh")
    script_content = _validator_command_template.format(
        validator = ctx.executable._json_comparator.short_path,
        generated = generated_file.short_path,
        golden = golden_file.short_path,
    )
    ctx.actions.write(script, script_content, is_executable = True)
    return script
