# Pigweed RPC Example

This connects to the server side of Pigweed's [local RPC example](https://pigweed.dev/pw_hdlc/rpc_example/#local-rpc-example-project).

Get the server side built and running.

Make sure you are running your emulator with networking, and find your emulator's IPv6 address with:
```
ip addr show
```

Put this address into `kEchoHost` in `pw_rpc_echo.cc`.

Build your tree and launch the component with:
```
ffx component run --recreate --follow-logs /core/ffx-laboratory:hello-world \
  'fuchsia-pkg://fuchsia.com/pw_rpc_echo#meta/pw_rpc_echo.cm'
```
