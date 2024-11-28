# Test execution

## Set up
```shell
$ fx set core.x64 \
    --with-host //src/developer/ffx/tests/e2e/python:tests

$ fx build
```

## Local execution
```shell
$ fx test //src/developer/ffx/tests/e2e/python:ffx_host_tool_e2e_test \
    --e2e --output
```
or
```shell
$ fx test //src/developer/ffx/tests/e2e/python:ffx_strict_host_tool_e2e_test \
    --e2e --output
```
