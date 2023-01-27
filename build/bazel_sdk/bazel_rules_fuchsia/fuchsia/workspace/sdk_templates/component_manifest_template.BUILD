load(
    "@rules_fuchsia//fuchsia:defs.bzl",
    "fuchsia_component_manifest_shard",
    "fuchsia_only_target",
)

fuchsia_component_manifest_shard(
    name = "{{name}}",
    include_path = "{{include_path}}",
    src = "{{source}}",
    target_compatible_with = fuchsia_only_target(),
)
