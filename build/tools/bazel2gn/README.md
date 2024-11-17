# Overview

bazel2gn is a tool for syncing build targets between BUILD.bazel and BUILD.gn
files in fuchsia.git. It works by converting build targets from BUILD.bazel
files and replacing the corresponding targets in BUILD.gn.

# Motivation

This tool is created to facilitate the GN to Bazel migration in fuchsia.git.
During the migration, it is unavoidable to have targets that need to be
buildable in both GN and Bazel, and it is costly to manually sync those targets.
This tool allows us to maintain the source of truth in Bazel, and automatically
sync those targets to GN.

# Design ideas

There are two main sources of differences between BUILD.gn and BUILD.bazel:

*   Differences between GN templates and Bazel rules for the same target;
*   Differences between Starlark and GN languages.

Examples of these differences and ideas for tackling them during conversion are
discussed in the sections below. For implementation details, please refer to the
Go sources in this directory.

## GN templates vs Bazel rules

It's very common for GN templates and Bazel rules to have different fields for
the same target type. Some of them are simple naming differences, for example
`srcs` in Bazel rules are usually spelled `sources` in GN templates. This can be
converted with hardcoded mappings. Others are more complicated, for example,
`go_library` from `rules_go` in Bazel requires sources for [embed][goembed] to
be specified in [`embedsrcs`][embedsrcs], and our `go_library` in GN simply
mixes embed sources with other sources in `sources`. To bridge this gap, it is
preferred to modify template implementation in GN while maintaining backward
compatibility, because:

*   The build team owns most of the GN templates, making them easier to change;
*   We'll eventually remove the GN targets, so it's better to keep Bazel targets
    as idiomatic as possible.

## Starlark vs GN

[Starlark][starlark] is the chosen language of Bazel for defining build targets,
while GN has its [own language][gnreference] for the same purpose. Starlark is
more declarative than GN. For example, to conditionally build a list in GN, you
can use if statements to imperatively add elements to the list, while in Bazel
this is done by concatenating results returned by the `select` function.

### Conditional targets

It's common to have targets in the build graph that are conditionally defined.
For example, host tools should only be built for the host. In GN, this is
expressed with if clauses. In Bazel, because if clauses are not supported in
BUILD.bazel files, this is done by setting the `target_compatible_with` field
when defining targets. So to do the conversion from Bazel to GN, `bazel2gn`
needs recognize `target_compatible_with` fields in Bazel, and know how to
convert its value to GN if conditions. For example, `target_compatible_with =
HOST_CONSTRAINTS` in Bazel maps to `if (is_host) { ... }` in GN. Note
`target_compatible_with` is a list, so it's possible for it to be converted to
several GN conditions ored together.

[embedsrcs]: https://github.com/bazel-contrib/rules_go/blob/master/docs/go/core/rules.md#go_library-embedsrcs
[goembed]: https://pkg.go.dev/embed
[starlark]: https://bazel.build/rules/language
[gnreference]: https://gn.googlesource.com/gn/+/main/docs/reference.md
