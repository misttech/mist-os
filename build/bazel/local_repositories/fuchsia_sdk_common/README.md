This directory is used to define a local repository for the
Fuchsia in-tree build that only exposes the common definitions
from the Fuchsia Bazel SDK rules.

Apart from the top-level WORKSPACE.bazel file, it should only
contain a "common" symlink that points to
//build/bazel_sdk/bazel_rules_fuchsia/common.

See https://fxbug.dev/42077053 for context.
