A series of host Bazel test targets that must *not* be wrapped
by a corresponding target. See https://fxbug/dev/349341932.

These can be built and run directly after `fx gen` with the
following command:

```
fx bazel test --config=no_sdk //build/bazel/host_tests/...
```
