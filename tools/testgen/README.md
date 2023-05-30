# testgen

testgen generates a Fuchsia test.

## Usage

```sh
fx testgen --help
```

Prefer running this tool from the root directory of the Fuchsia checkout.

## Quickstart

To generate an integration test for a Fuchsia component use:

```sh
# Generate an integration test
fx testgen -v integration_test \
    --test-root ./src/my/tests \
    --component-manifest ./src/my/component/meta/my_component.cml

# Run the test (assumes you have a device & package server running).
fx set core.x64 --with //src/my/tests
fx build
fx test
```

The generated test should build, run and pass. The generated code will contain
lines that look like this:

```rust
// FIXME: ...
```

These lines indicate places where the User should add their own code. `grep` for
these lines to get started.
