load(
    "@rules_fuchsia//fuchsia:defs.bzl",
    "fuchsia_package_resource_group",
    "fuchsia_only_target",
    "fuchsia_select",
)

fuchsia_package_resource_group(
    name = "dist",
    srcs = fuchsia_select({{srcs}}),
    dest = "lib",
    strip_prefix = "{{strip_prefix}}",
    target_compatible_with = fuchsia_only_target(),
)
