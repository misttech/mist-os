# ffx_e2e_emu

A Rust library for end-to-end testing of ffx tool and plugin behavior. Starts an
isolated emulator instance with the system image from the main build and allows
issuing ffx & ssh commands to it.

## Build setup

To avoid dependency cycles in the build graph you must place your tests outside
of the usual chain of `test` groups that get depended upon by the system image
assembly.

Add host test binaries that depend on the library to
`//src/developer/ffx:host_tests` and add any Fuchsia package dependencies
required by your test to `//src/developer/ffx:package_deps_for_host_tests`,
usually by adding similarly-named groups to the build for your test and the
BUILD.gn files in any parent directories. See [existing users] of this library
for examples.

[existing users]: https://cs.opensource.google/search?q=%2F%2Fsrc%2Fdeveloper%2Fffx%2Flib%2Fe2e_emu&ss=fuchsia

## Isolated emulator tests are slow

You should be sparing in the number of tests you write using this library for
2 reasons:

1. spinning up a full Fuchsia instance for a test can take 20-30 seconds
2. only one ffx isolate can be in use at once, serializing test execution
