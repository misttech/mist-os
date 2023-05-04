The definitions in this directory are shared by the Fuchsia
platform (in-tree) workspace, as well as other out-of-tree
Bazel based projects. In other words:

- They MUST NOT depend on definitions outside of this
  directory.

- They should not assume to be in any specific workspace
  (so no explicit reference to `@fuchsia_sdk` or
  `@fuchsia_workspace` should exist in this directory.
  Instead, repository labels should be provided as
  repository / function arguments explicitly and specified
  by the callers.

