load(
    "@rules_fuchsia//fuchsia:defs.bzl",
    "fuchsia_only_target",
)

cc_import(
    name = "prebuilts",
    {{library_type}}_library = "{{link_lib}}",
    target_compatible_with = fuchsia_only_target(),
)