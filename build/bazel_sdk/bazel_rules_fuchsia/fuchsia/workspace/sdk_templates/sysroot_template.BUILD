alias(
    name = "dist",
    actual = select({
        "@rules_fuchsia//fuchsia/constraints:is_fuchsia_arm64": "//{{relative_dir}}/arm64:dist",
        "@rules_fuchsia//fuchsia/constraints:is_fuchsia_x64": "//{{relative_dir}}/x64:dist",
    }),
)
