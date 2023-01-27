load(
    "@rules_fuchsia//fuchsia:defs.bzl",
    "fuchsia_package_resource",
    "fuchsia_only_target",
)

fuchsia_package_resource(
    name = "dist",
    src = "{{dist_lib}}",
    dest = "{{dist_path}}",
    target_compatible_with = fuchsia_only_target(),
)