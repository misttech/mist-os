This directory contains:

- `repository_rules.bzl` which defines the `fuchsia_idk_repository()`
  repository rule.

- `generate_repository.py`: A script invoke by the repository rule
  that processes an input IDK directory, that uses the official IDK
  layout, and generates an output IDK directory, whose metadata
  files contain Bazel labels for all Ninja artifacts.

- `generate_repository_test.py`: A unit-test for the classes
  defined in `generate_repository.py`.

