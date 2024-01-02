load(
    "@fuchsia_sdk//fuchsia:defs.bzl",
    "fuchsia_package_resource",
)

fuchsia_package_resource(
    name = "dist",
    src = "{{dist_lib}}",
    dest = "{{dist_path}}",
    target_compatible_with = [ "@platforms//os:fuchsia" ],
)
