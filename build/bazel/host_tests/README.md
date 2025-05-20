A series of host Bazel test targets that must *not* be wrapped
by a corresponding target. See https://fxbug/dev/349341932.

These can be built and run directly after `fx gen` with either of the
following commands:

```
fx bazel test --platforms=//build/bazel/platforms:host //build/bazel/host_tests/...
```
```
fx bazel test --config=host //build/bazel/host_tests/...
```
