# testgen

testgen generates boilerplate code for Fuchsia integration tests.

## Usage

```sh
fx testgen integration_test --component-manifest /path/to/.cml --test-root /src/my/tests
```

It's recommended to run this tool from the root directory (`${FUCHSIA_DIR}`) of the Fuchsia checkout.

## Quickstart

To generate an integration test for a Fuchsia component use:

```sh
# Generate an integration test
fx testgen integration_test \
    --test-root ${FUCHSIA_DIR}/src/my/tests \
    --component-manifest ${FUCHSIA_DIR}/src/my/component/meta/my_component.cml

# Run the test (assumes you have a device & package server running).
fx set core.x64 --with //src/my/tests
fx build
fx test
```

The generated test should build, run and pass. The generated test root path will contain paths and files similar to `${FUCHSIA_DIR}/tools/testgen/testdata/goldens/integration_test`
