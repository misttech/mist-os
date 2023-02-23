load(
    "@fuchsia_sdk//fuchsia:defs.bzl",
    "fuchsia_package_resource_group",
    "fuchsia_select",
)

fuchsia_package_resource_group(
    name = "dist",
    srcs = fuchsia_select({{srcs}}),
    dest = "lib",
    strip_prefix = "{{strip_prefix}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
