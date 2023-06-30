# Pigweed RPC Example

This connects to the server side of Pigweed's [local RPC example](https://pigweed.dev/pw_hdlc/rpc_example/#local-rpc-example-project).

Get the server side built and running, and then put your IPv4 address from:
```
ip addr show
```
into `kEchoHost` in `pw_rpc_echo.cc`.

Build your tree and launch the component with:
```
ffx component run --recreate /core/ffx-laboratory:hello-world \
  'fuchsia-pkg://fuchsia.com/pw_rpc_echo#meta/pw_rpc_echo.cm'
```
