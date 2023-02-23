load(
    "@fuchsia_sdk//fuchsia:defs.bzl",
    "fuchsia_select",
)

alias(
    name = "dist",
    actual = fuchsia_select({
        "@fuchsia_clang//:arm_build": "//{{relative_dir}}/arm64:dist",
        "@fuchsia_clang//:x86_build": "//{{relative_dir}}/x64:dist",
    }),
)
