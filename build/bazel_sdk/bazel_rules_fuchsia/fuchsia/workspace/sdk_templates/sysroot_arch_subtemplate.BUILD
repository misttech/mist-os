load(
    "@rules_fuchsia//fuchsia:defs.bzl",
    "fuchsia_package_resource_group",
)

fuchsia_package_resource_group(
    name = "dist",
    srcs = select({{srcs}}),
    dest = select({{lib_path}}),
    strip_prefix = "{{strip_prefix}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
