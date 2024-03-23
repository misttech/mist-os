# Fuchsia Controller tutorial

This tutorial walks through the steps on how to write a simple script that uses
Fuchsia Controller (`fuchsia-controller`) in the Fuchsia source checkout
(`fuchsia.git`) setup.

Fuchsia Controller consists of a set of libraries that allow users to connect
to a Fuchsia device and interact with the device using FIDL. Fuchsia Controller
was initially created for testing. But it is also useful for creating scriptable
code that interacts with FIDL interfaces on a Fuchsia device. For instance,
users may use Fuchsia Controller to write a script that performs simple device
interactions without having to write an `ffx` plugin in Rust.

The main two parts of Fuchsia Controller are:

- The `fuchsia-controller.so` file (which includes a
  [header][fuchsia-controller-header-file] for the ABI)
- Higher level language bindings (which are built on top of the `.so` file
  using the ABI)

  Currently, Fuchsia Controller's higher level language bindings are written
  in Python only.

The quickest way to use Fuchsia Controller is to write a Python script that
uses the `fuchsia-controller` code. In the Fuchsia source checkout setup,
you can build your Python binary into a `.pyz` file, which can then be
executed from the `out` directory (for instance, `$FUCHSIA_DIR/out/default`).

To write your first Fuchsia Controller script, the steps are:

1. [Prerequisites](#prerequisites).
1. [Update dependencies in BUILD.gn](#update-dependencies-in-buildgn).
1. [Write your first program](#write-your-first-program).
1. [Communicate with a Fuchsia device](#communicate-with-a-fuchsia-device).
1. [Implement a FIDL server](#implement-a-fidl-server).

If you run into bugs or have questions or suggestions, please
[file a bug][file-a-bug].

## Prerequisites {:.numbered}

This tutorial requires the following prerequisite items:

- You need to use the Fuchsia source checkout (`fuchsia.git`) development
  environment.

- You need a Fuchsia device running. This can either be a physical device
  or an emulator.

- This device must have a connection to `ffx` and have the remote control
  service (RCS) connected properly.

  If running `ffx target list`, the field under `RCS` must read `Y`,
  for example:

  ```none {:.devsite-disable-click-to-copy}
  NAME                    SERIAL       TYPE       STATE      ADDRS/IP                       RCS
  fuchsia-emulator        <unknown>    Unknown    Product    [fe80::5054:ff:fe63:5e7a%4]    Y
  ```

  (For more information, see
  [Interacting with target devices][interact-with-target-devices].)

- To start the Fuchsia emulator with networking enabled but without graphical
  user interface support, run `ffx emu start --headless`. (For more
  information, see [Start the Fuchsia emulator][femu-guide].)

- Your device must be running a `core` [product][product-config] at a minumim.

## Update dependencies in BUILD.gn {:#update-dependencies-in-buildgn .numbered}

Update a `BUILD.gn` file to include the following dependencies:

```none {:.devsite-disable-click-to-copy}
import("//build/python/python_binary.gni")

assert(is_host)

python_binary("your_binary") {
    main_source = "path/to/your/main.py"
    deps = [
        "//src/developer/ffx:host",
        "//src/developer/ffx/lib/fuchsia-controller:fidl_bindings",
        "//src/developer/ffx/lib/fuchsia-controller:fuchsia_controller_py",
    ]
}
```

The `fidl_bindings` rule includes the necessary Python and `.so` binding code.
The `ffx` tool must also be included to enable the `ffx` daemon to connect to
your Fuchsia device.

## Write your first program {:#write-your-first-program .numbered}

In this section, we create a simple program that doesn't yet connect to a
Fuchsia device, but connect to the `ffx` daemon to verify that the device is
up and running. To do this, we leverage the existing `ffx` FIDL libraries for
interacting with the daemon, which is defined in `//src/developer/ffx/fidl`.

### Include FIDL dependencies {:#include-fidl-dependencies .numbered}

Fuchsia Controller uses the FIDL Intermediate Representation (FIDL IR) to
generate its FIDL bindings at runtime. So you need to include the following
dependency in your `BUILD.gn` for the `fidlc` target to create these FIDL
bindings:

```gn
"//src/developer/ffx/fidl:fuchsia.developer.ffx($fidl_toolchain)"
```

This also requires including an import of the `$fidl_toolchain` variable:

```gn
import("//build/fidl/toolchain.gni")
```

If you're writing a test, you need to include the host test data (which will
allow infra tests to run correctly, given they need access to the IR on test
runners as well), for example:

```gn
"//src/developer/ffx/fidl:fuchsia.developer.ffx_host_test_data"
```

Including the host test data rule will also include the FIDL IR, so no need
to include both dependencies.

### Add the Python import block {:#add-the-python-imnport-block .numbered}

Once all dependencies are all included, we can add the following libraries
in the Python main file:

```py
from fuchsia_controller_py import Context, IsolateDir
import fidl.fuchsia_developer_ffx as ffx
import asyncio
```

The sections below cover each library in this code block.

#### Context and IsolateDir

```py
from fuchsia_controller_py import Context, IsolateDir
```

The first line includes a `Context` object, which provides the context from
which a user might run an `ffx` command. Plus, you can do much more with this
object because it also provides connections the following:

- The `ffx` daemon
- A Fuchsia target

The `IsolateDir` object is related to `ffx` isolation, which refers to
running the `ffx` daemon in a way that all its metadata (for instance, config
values) is contained under a specific directory. Isolation is primarily
intended for preventing pollution of `ffx`'s state as well as setting up less
active device discovery defaults (which can cause issues when running `ffx` in
testing infrastructure).

`IsolateDir` is optional for general purpose commands, but is required if you
intend to use your program for testing. An `IsolateDir` object creates (and
points to) a directory that allows an isolated `ffx` daemon instance to run.
(For more information on `ffx` isolation, see
[Integration testing][integration-testing].)

An `IsolateDir` object needs to be passed to a `Context` object during
initialization. An `IsolateDir` object may also be shared among `Context`
objects. The cleanup of an `IsolateDir` object, which also results in
the shutdown of the `ffx` daemon, occurs once the object is garbage
collected.

#### FIDL IR

```py
import fidl.fuchsia_developer_ffx as ffx
```

The second line comes from the FIDL IR code written in the previous section
above. The part written after `fidl.` (for instance, `fuchsia_developer_ffx`)
requires that the FIDL IR exists for the `fuchsia.developer.ffx` library.
This is the case for any FIDL import line. Importing
`fidl.example_fuchsia_library` requires that the FIDL IR for a library
named `example.fuchsia.library` has been generated. Using the `as` keyword
makes this library easy to use.

This `fuchsia.developer.ffx` library includes all the structures expected
from FIDL bindings, which is covered later in this tutorial.

#### asyncio

```py
import asyncio
```

The objects generated from FIDL IR use asynchronous bindings, which requires
use of the `asyncio` library. In this tutorial, we use the echo protocol
defined in [`echo.fidl`][echo-fidl].

### Write the main implementation {:.numbered}

Beyond the boilerplate of `async_main` and `main`, we're primarily interested
in the `echo_func` definition:

```py
async def echo_func():
   isolate = IsolateDir()
   config = {"sdk.root": "."}
   ctx = Context(config=config, isolate_dir=isolate)
   echo_proxy = ffx.Echo.Client(ctx.connect_daemon_protocol(ffx.Echo.MARKER))
   echo_string = "foobar"
   print(f"Sending string for echo: {echo_string}")
   result = await echo_proxy.echo_string(value="foobar")
   print(f"Received result: {result.response}")


async def async_main():
    await echo_func()


def main():
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
```

The `config` object created and passed to the `Context` object is necessary
because of the isolation in use. When it's no longer applicable to use
isolation with `ffx`'s default config (by default `ffx` knows where to find
the SDK in the Fuchsia source checkout setup), any config values that you
wish to use must be supplied to the `Context` object.

### Run the code {:.numbered}

Before we can run the code, we must build it first. The `BUILD.gn` file
may look similar to the following:

```gn
import("//build/python/python_binary.gni")

assert(is_host)

python_binary("example_echo") {
    main_source = "main.py"
    deps = [
        "//src/developer/ffx:host",
        "//src/developer/ffx/lib/fuchsia-controller:fidl_bindings",
        "//src/developer/ffx/fidl:fuchsia.developer.ffx_compile_fidlc($fidl_toolchain)",
    ]
}
```

Let's say this `BUILD.gn` is in the `src/developer/example_py_thing`
directory. Then with the correct `fx set` in place, you can build
this code using the host target. If your host is `x64`, the build
command may look like:

```posix-terminal
fx build host_x64/obj/src/developer/example_py_thing/example_echo.pyz
```

One the build is complete, you can find the code in the `out`
directory (to be precise, `out/default` by default). And you can run
the `.pyz` file directly from that directory. It is important to use
the full path from your `out/default` directory so that the `pyz` file
can locate and open the appropriate `.so` files, for example:

```sh {:.devsite-disable-click-to-copy}
$ cd $FUCHSIA_DIR/out/default
$ ./host_x64/obj/src/developer/example_py_thing/example_echo.pyz
Sending string for echo: foobar
Received result: foobar
$
```

## Communicate with a Fuchsia device {:#communicate-with-a-fuchsia-device .numbered}

If the code builds and runs so far, we can start writing code that speaks
to Fuchsia devices through FIDL interfaces. Most code is similar, but
there are some subtle differences to cover in this section.

### Find component monikers {:.numbered}

To bind to Fuchsia components, it is currently necessary to know the component's
moniker. This can be done using `ffx`. To get the moniker for the build info
provider, for example:

```posix-terminal
ffx component capability fuchsia.buildinfo.Provider
```

This command will print output similar to the following:

```sh {:.devsite-disable-click-to-copy}
Declarations:
  `core/build-info` declared capability `fuchsia.buildinfo.Provider`

Exposes:
  `core/build-info` exposed `fuchsia.buildinfo.Provider` from self to parent

Offers:
  `core` offered `fuchsia.buildinfo.Provider` from child `#build-info` to child `#cobalt`
  `core` offered `fuchsia.buildinfo.Provider` from child `#build-info` to child `#remote-control`
  `core` offered `fuchsia.buildinfo.Provider` from child `#build-info` to child `#sshd-host`
  `core` offered `fuchsia.buildinfo.Provider` from child `#build-info` to child `#test_manager`
  `core` offered `fuchsia.buildinfo.Provider` from child `#build-info` to child `#testing`
  `core` offered `fuchsia.buildinfo.Provider` from child `#build-info` to child `#toolbox`
  `core/sshd-host` offered `fuchsia.buildinfo.Provider` from parent to collection `#shell`

Uses:
  `core/remote-control` used `fuchsia.buildinfo.Provider` from parent
  `core/sshd-host/shell:sshd-0` used `fuchsia.buildinfo.Provider` from parent
  `core/cobalt` used `fuchsia.buildinfo.Provider` from parent
```

The moniker you want is under the `Exposes` declaration: `core/build-info`.

### Get build information {:.numbered}

We can start simple by getting a device's build information.

To start, we need to include dependencies for the build info FIDL protocols:

```gn
"//sdk/fidl/fuchsia.buildinfo:fuchsia.buildinfo_compile_fidlc($fidl_toolchain)"
```

We then need to write code for getting a proxy from a Fuchsia device.
Currently, this is done by connecting to the build info moniker (though
this is due to change soon):

```py
isolate = IsolateDir()
config = {"sdk.root": "."}
target = "foo-target-emu" # Replace with the target nodename.
ctx = Context(config=config, isolate_dir=isolate, target=target)
build_info_proxy = fuchsia_buildinfo.Provider.Client(
    ctx.connect_device_proxy("/core/build-info", fuchsia_buildinfo.Provider.MARKER))
build_info = await build_info_proxy.get_build_info()
print(f"{target} build info: {build_info}")
```

If you were to run the above code, it would print something like below:

```sh {:.devsite-disable-click-to-copy}
foo-target-emu build info: ProviderGetBuildInfoResponse(build_info=BuildInfo(product_config='core', board_config='x64', version='2023-08-18T23:28:37+00:00', latest_commit_date='2023-08-18T23:28:37+00:00'))
```

If you were to continue this, you could create something akin to the
`ffx target show` command:

```py
results = await asyncio.gather(
    build_info_proxy.get_build_info(),
    board_proxy.get_info(),
    device_proxy.get_info(),
    ...
)
```

Since each invocation to a FIDL method returns a co-routine, they can be
launched as tasks and awaited in parallel, as you would expect with other
FIDL bindings.

### Reboot a device {:.numbered}

There's more than one way to reboot a device. One approach to reboot a
device is to connect to a component running the
`fuchsia.hardware.power.statecontrol/Admin` protocol, which can be found
under `/bootstrap/shutdown_shim`.

With this approach, the protocol is expected to exit mid-execution of the
method with a `PEER_CLOSED` error:

```py
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="src/developer/ffx/lib/fuchsia-controller/end_to_end_tests/mobly/reboot_test.py" region_tag="reboot_example" %}
```

However, a challenging part comes afterward when we need to determine
whether or not the device has come back online. This is usually done by
attempting to connect to a protocol (usually the `RemoteControl` protocol)
until a timeout is reached.

A different approach, which results in less code, is to connect to the
`ffx` daemon's `Target` protocol:

```py
ch = ctx.connect_target_proxy()
target_proxy = fuchsia_developer_ffx.Target.Client(ch)
await target_proxy.reboot(state=fuchsia_developer_ffx.TargetRebootState.PRODUCT)
```

### Run a component {:.numbered}

Note: This section may be subject to change depending on the development
in the component framework.

You can use the `RemoteControl` protocol to start a component, which involves
the following steps:

1. Connect to the lifecycle controller:

   ```py
   ch = ctx.connect_to_remote_control_proxy()
   remote_control = fuchsia_developer_remotecontrol.RemoteControl.Client(ch)
   client, server = fuchsia_controller_py.Channel.create()
   await remote_control.root_lifecycle_controller(server=server.take())
   lifecycle_ctrl = fuchsia_sys2.LifecycleController.Client(client)
   ```

2. Attempt to start the instance of the component:

   ```py
   client, server = fuchsia_controller_py.Channel.create()
   await lifecycle_ctrl.start_instance("some_moniker", server=server.take())
   binder = fuchsia_component.Binder.Client(client)
   ```

   The `binder` object lets the user know whether or not the component
   remains connected. However, it has no methods. Support to determine
   whether the component has become unbound (using the binder protocol)
   is not yet implemented.

### Get a snapshot {:.numbered}

Getting a snapshot from a fuchsia device involves running a snapshot and
binding a `File` protocol for reading:

```py
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="src/developer/ffx/lib/fuchsia-controller/end_to_end_tests/mobly/target_identity_tests.py" region_tag="snapshot_example" %}
```

## Implement a FIDL server {:#implement-a-fidl-server .numbered}

An important task for Fuchsia Controller (either for handling passed bindings
or for testing complex client side code) is to run a FIDL server. For all
FIDL protocols covered in this tutorial, there is a client that accepts
a channel. For this, you need to use the `Server` class.

In this section, we return to the `echo` example and implement an `echo` server.
The functions you need to override are derived from the FIDL file definition. So
the `echo` server (using the `ffx` protocol) would look like below:

```py
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="src/developer/ffx/lib/fuchsia-controller/tests/server.py" region_tag="echo_server_impl" %}
```

To make a proper implementation, you need to import the appropriate libraries.
As before, we will import `fidl.fuchsia_developer_ffx`. However, since we're
going to run an `echo` server, the quickest way to test this server is to use
a `Channel` object from the `fuchsia_controller_py` library:

```py
import fidl.fuchsia_developer_ffx as ffx
from fuchsia_controller_py import Channel
```

This `Channel` object behaves similarly to the ones in other languages.
The following code is a simple program that utilizes the `echo` server:

```py
import asyncio
import unittest
import fidl.fuchsia_developer_ffx as ffx
from fuchsia_controller_py import Channel


{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="src/developer/ffx/lib/fuchsia-controller/tests/server.py" region_tag="echo_server_impl" %}


class TestCases(unittest.IsolatedAsyncioTestCase):

    async def test_echoer_example(self):
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="src/developer/ffx/lib/fuchsia-controller/tests/server.py" region_tag="use_echoer_example" %}
```

There are a few things to note when implementing a server:

* Method definitions can either be `sync` or `async`.
* The `serve()` task will process requests and call the necessary method in
  the server implementation until either the task is completed or the
  underlying channel object is closed.
* If an exception occurs when the serving task is running, the client
  channel receives a `PEER_CLOSED` error. Then you must check the result
  of the serving task.
* Unlike Rust's async code, when creating an async task, you must keep
  the returned object until you're done with it. Otherwise, the task may
  be garbage collected and canceled.

<!-- Reference links -->

[fuchsia-controller-header-file]: /src/developer/ffx/lib/fuchsia-controller/cpp/abi/fuchsia_controller.h
[file-a-bug]: https://issuetracker.google.com/issues/new?component=1378581&template=1840403
[interact-with-target-devices]: /docs/development/tools/ffx/getting-started.md#interacting_with_target_devices
[femu-guide]: /docs/get-started/set_up_femu.md
[product-config]: /docs/development/build/build_system/boards_and_products.md
[integration-testing]: /docs/development/tools/ffx/development/integration_testing/README.md
[echo-fidl]: /src/developer/ffx/fidl/echo.fidl
