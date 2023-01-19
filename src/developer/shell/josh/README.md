# Josh: Interact with Fuchsia through JavaScript.

This directory is for development of Josh, a Fuchsia JavaScript shell and interpreter.

A less generic name, and more content, is pending.

The current shell is based on JavaScript.

Currently, invoking FIDL calls (and wrappers around FIDL calls) requires
the use of `Promise`s.  You can invoke FIDL calls on protocols available from
`/svc` easily by saying something like:

```
svc.fuchsia_kernel_DebugBroker.SendDebugCommand("threadstats")
```

This returns a `Promise`.  Followup actions can be taken in the `Promise`'s then
clause.  You can also use `async`/`await`, although this is not supported for
direct invocation from the command line.

Arbitrary FIDL requests (those that do not depend on `/svc`) can be sent
using the `fidl.Request` and `fidl.ProtocolClient` API.  For example, if you have a
channel to a directory called dirChannel, and you want to open a path within that
directory, you can say:

```
let dirClient = new fidl.ProtocolClient(dirChannel, fidling.fuchsia_io.Directory);

const request = new fidl.Request(fidling.fuchsia_io.Node);
pathClient = request.getProtocolClient();
let openedPromise = pathClient.OnOpen((args) => {
  return args;
});
dirClient.Open(
    fidling.fuchsia_io.OPEN_RIGHT_READABLE | fidling.fuchsia_io.OPEN_FLAG_DESCRIBE, 0,
    path, request.getChannelForServer());
let args = await openedPromise;

// Manipulate args.s or args.info.

fidl.ProtocolClient.close(request.getProtocolClient());
```

Note that available FIDL libraries are available off of a global object called
`fidling`.  If a FIDL library you want is not available, please contact the OWNERS
of this directory.

If you want to try the shell, add the package "//src/developer/shell:josh" to
the packages available, shell into the device, and type "josh".

# Create A JavaScript Running Environment

The following is an example of creating a **JavaScript environment** in the Fuchsia
component `factory_josh` which allows JavaScript to run inside. This component contains:

- A list of FIDL IRs (`fidl_json`) to be supported.
- Startup modules (`js_startup_libs`) for the factory environment.
- Standard Josh (`//src/developer/shell/josh:bin`)

The component can be run by others to execute the test scripts sent in, so the component
does not include any application script.

    import("//src/developer/shell/generate_fidl_json.gni")

    # Select FIDL IRs to be supported (use distribution_fidl_json from generate_fidl_json.gni)
    distribution_fidl_json("fidl_json") {
      fidl_deps = [
        "//sdk/fidl/fuchsia.buildinfo",
        "//sdk/fidl/fuchsia.device.manager",
        "//sdk/fidl/fuchsia.hardware.backlight",
        "//sdk/fidl/fuchsia.hardware.cpu.ctrl",
        "//sdk/fidl/fuchsia.hardware.gpio",
        "//sdk/fidl/fuchsia.hardware.i2c",
        "//sdk/fidl/fuchsia.hardware.light",
        "//sdk/fidl/fuchsia.hardware.spi",
        "//sdk/fidl/fuchsia.io",
        "//sdk/fidl/fuchsia.sysinfo",
      ]
    }

    # Include environment startup modules
    resource("js_startup_libs") {
      sources = [
        "startup/hw.js",
        "startup/startup.json",
      ]
      outputs = [ "data/lib/startup/{{source_file_part}}" ]
    }

    fuchsia_component("factory_josh") {
      manifest = "meta/factory_josh.cml"  # Component manifest for capability routing
      deps = [
        ":fidl_json",                     # Supported FIDL IRs
        ":js_startup_libs",               # Startup modules
        "//src/developer/shell/josh:bin", # Include standard Josh
      ]
    }

When exploring the created component (environment), running `josh` will land on the
Javascript console:

    $ ffx component explore <COMPONENT URL/moniker/instance ID> -l namespace
    $ josh
    QuickJS - Type "\h" for help
    qjs > console.log("Hello World!");
    Hello World!