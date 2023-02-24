alias(
    name = "dist",
    actual = select({
        "@fuchsia_sdk//fuchsia/constraints:is_fuchsia_arm64": "//{{relative_dir}}/arm64:dist",
        "@fuchsia_sdk//fuchsia/constraints:is_fuchsia_x64": "//{{relative_dir}}/x64:dist",
    }),
)
