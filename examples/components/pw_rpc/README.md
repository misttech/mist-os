# Pigweed RPC Example

This example demonstrates a simple example of integration between the Fuchsia
component framework and an offloaded program that uses Pigweed.

-   `pw_system_demo`: A binary that can be built from the Pigweed repo, found at
    `//pw_system`. It runs a Pigweed RPC server that exposes an `Echo` and a
    `Logs` service.
-   `pw_runner`: A Fuchsia component runner that hosts components which bridge
    between Fuchsia and offloaded programs that serve Pigweed RPC.
-   `pw_rpc_server`: A Fuchsia component that uses `pw_runner`, which provides a
    proxy to `pw_system_demo`.
-   `pw_rpc_client`: A Fuchsia component that is the client of `pw_rpc_server`.
    It makes an `Echo` RPC to the server.

To bridge between Fuchsia and the offloaded program, `pw_runner` implements the
following functionality:

-   Forwards the logs emitted by the offloaded program to Fuchsia logs
    attributed to the component using the runner.
-   Implements a protocol, `fidl.example.pigweed/RemoteEndpoint`, that clients
    can use to obtain a connection to the remote endpoint. This protocol is
    routed from the component using the runner to the client component.
-   Binds the lifetime of the component using the runner to the lifetime of the
    runner's connection to the offloaded program.

## Instructions

### Configure your IP address

Make sure you are running your emulator
[in Tun/Tap mode](https://fuchsia.dev/fuchsia-src/development/build/emulator?hl=en#networking),
and find your emulator's IPv6 address with:

```
ip addr show
```

Replace the entry in the `host` field in
[//examples/components/pw_rpc/server/meta/pw_rpc_server.cml][pw_rpc_server.cml]
with this address.

### Build and run pigweed example

First, you need a Pigweed checkout. You have two options for this:

- Follow the pigweed.dev [Getting Started](https://pigweed.dev/docs/getting_started.html)
  instructions to obtain a fresh checkout.
- `cd` to the submodule that's part of fuchsia.git, `//third_party/pigweed/src`.

After you follow those instructions to setup a checkout, activate the pigweed
development environment.

```sh
$ cd ${PIGWEED_DIR}
$ . ./activate.sh
```

You'll need to enable multi-endpoint mode on `pw_system` for the example to
work. For more information on this, see
[Multi-endpoint mode](https://pigweed.dev/pw_system/#multi-endpoint-mode).

```sh
$ gn args out

# Set build arguments here. See `gn help buildargs`.
pw_system_USE_MULTI_ENDPOINT_CONFIG = true
```

In this terminal, run the following commands to start the Pigweed example binary
that we'll use as the remote endpoint:

```sh
$ ninja -C out pw_system_demo
$ ./out/host_device_simulator.speed_optimized/obj/pw_system/bin/system_example
```

You should see this in the console to indicate the program is waiting for
connections:

```
Awaiting connections on port 33000
```

### Build and run Fuchsia example

First, make sure the example is in your `fx args`:

```sh
$ fx set core.x64 --with //examples/components/pw_rpc`
```

Build the tree:

```sh
$ cd ${FUCHSIA_DIR}
$ fx build
```

In a terminal (different from the one running `pw_system_demo`), launch the
`pw_rpc_realm` component, which bundles the client and server together:

```
ffx component run --recreate -f /core/ffx-laboratory:pw_rpc \
  'fuchsia-pkg://fuchsia.com/pw_rpc_realm#meta/pw_rpc_realm.cm'
```

### Observe the behavior

The output of `ffx component run` should include logs that indicate what is
happening in the client and server. For example:

```
[00039.842753][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(90)] Starting up.
[00039.853834][pw_runner] INFO: [examples/components/pw_rpc/runner/main.cc(36)] Starting up.
[00039.854959][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(106)] Sending echo request.
[00039.854977][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(118)] Processing packets.
[00039.856143][pw_server] INFO: [pw_system/init.cc(41)] System init
[00039.856369][pw_server] INFO: [pw_system/example_user_app_init.cc(28)] Pigweed is fun!
[00039.856589][pw_server] INFO: [pw_system/hdlc_rpc_server.cc(113)] Running RPC server
[00039.856683][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(110)] Received echo reply msg=Hello, Pigweed
[00039.856695][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(121)] Done.
```

These lines indicates that the client made an RPC and received the response:

```
[00039.856683][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(110)] Received echo reply msg=Hello, Pigweed
[00039.856695][pw_client] INFO: [examples/components/pw_rpc/client/pw_rpc_client.cc(121)] Done.
```

These lines contain the logs from `pw_system_demo` Notice how they are
attributed to the `pw_server` proxy component.

```
[00039.856143][pw_server] INFO: [pw_system/init.cc(41)] System init
[00039.856369][pw_server] INFO: [pw_system/example_user_app_init.cc(28)] Pigweed is fun!
[00039.856589][pw_server] INFO: [pw_system/hdlc_rpc_server.cc(113)] Running RPC server
```

Note: the Fuchsia timestamps do not reflect the Pigweed timestamps currently,
which is why the server log appears second even though temporally it happened
first.

Verify that the server component is still running:

```sh
$ ffx component show core/ffx-laboratory:pw_rpc/pw_server
                 Moniker:  core/ffx-laboratory:pw_rpc/pw_server
                     URL:  pw_rpc_server#meta/pw_rpc_server.cm
             Environment:  pw_env
             Instance ID:  None
         Component State:  Resolved
            Resolved URL:  pw_rpc_server#meta/pw_rpc_server.cm
  Namespace Capabilities:  /svc/fuchsia.logger.LogSink
    Exposed Capabilities:  fidl.examples.pigweed.RemoteEndpoint
             Merkle root:  d16ac772d07caae0b1f2a75a468f324a1c20d4f1801ad3065c3e39c1ccee0a47
         Execution State:  Running
            Start reason:  'core/ffx-laboratory:pw_rpc/pw_client' requested capability 'fidl.examples.pigweed.RemoteEndpoint'
   Outgoing Capabilities:  debug
                           fidl.examples.pigweed.RemoteEndpoint
                 Runtime:  Unknown
```

From the terminal running `pw_system_demo`, you should see a message reporting
the client's connection:

```
$ ./out/host_device_simulator.speed_optimized/obj/pw_system/bin/system_example
Awaiting connections on port 33000
Clients connected
```

### Terminating the example

You can terminate the `pw_system_demo` process and verify that gets reflected in
the server component:

```sh
$ ./out/host_device_simulator.speed_optimized/obj/pw_system/bin/system_example
Awaiting connections on port 33000
Clients connected
^C
```

```sh
$ ffx component show core/ffx-laboratory:pw_rpc/pw_server
                 Moniker:  core/ffx-laboratory:pw_rpc/pw_server
                     URL:  pw_rpc_server#meta/pw_rpc_server.cm
             Environment:  pw_env
             Instance ID:  None
         Component State:  Resolved
            Resolved URL:  pw_rpc_server#meta/pw_rpc_server.cm
  Namespace Capabilities:  /svc/fuchsia.logger.LogSink
    Exposed Capabilities:  fidl.examples.pigweed.RemoteEndpoint
             Merkle root:  d16ac772d07caae0b1f2a75a468f324a1c20d4f1801ad3065c3e39c1ccee0a47
         Execution State:  Stopped
```

You should also see a log message from the runner indicating this:

```
[00044.200376][pw_server] INFO: [examples/components/pw_rpc/runner/log_proxy.cc(247)] Connection to proxy has terminated. Exiting.
```
