# Write a minimal DFv2 driver

This guide walks through the steps involved in creating a minimal DFv2 driver.

The instructions in this guide are based on the minimal
[skeleton driver][skeleton-driver], which provides the minimum implementation
necessary to build, load, and register a new DFv2 driver in a Fuchsia system.

The steps are:

1. [Create a driver header file](#create-a-driver-header-file).
1. [Create a driver source file](#create-a-driver-source-file).
1. [Add the driver export macro](#add-the-driver-export-macro).
1. [Create a build file](#create-a-build-file).
1. [Write bind rules](#write-bind-rules).
1. [Create a driver component](#create-a-driver-component).

For more DFv2-related features, see [Additional tasks](#additional-tasks).

## Create a driver header file {:#create-a-driver-header-file .numbered}

To create a header file for your DFv2 driver, do the following:

1. Create a new header file (`.h`) for the driver (for example,
   `skeleton_driver.h`).

1. Include the following interface to the header file:

   ```cpp
   #include <lib/driver/component/cpp/driver_base.h>
   ```

1. Add an interface for the [`DriverBase`][driver-base] class,
   for example:

   ```cpp
   #include <lib/driver/component/cpp/driver_base.h>

   namespace skeleton {

   class SkeletonDriver : public fdf::DriverBase {
    public:
     SkeletonDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

     // Called by the driver framework to initialize the driver instance.
     zx::result<> SkeletonDriver::Start() override;
   };

   }  // namespace skeleton
   ```

   (Source: [`skeleton_driver.h`][skeleton-driver-h])

## Create a driver source file {:#create-a-driver-source-file .numbered}

To implement the basic methods for the `DriverBase` class,
do the following:

1. Create a new source file (`.cc`) for the driver (for example,
   `skeleton_driver.cc`).

1. Include the header file created for the driver, for example:

   ```cpp
   #include "skeleton_driver.h"
   ```

1. Implement the basic methods for the class, for example:

   ```cpp
   #include "skeleton_driver.h"

   namespace skeleton {

   SkeletonDriver::SkeletonDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
       : DriverBase("skeleton_driver", std::move(start_args),
             std::move(driver_dispatcher)) {
   }

   zx::result<> SkeletonDriver::Start() {
     return zx::ok();
   }

   }  // namespace skeleton
   ```

   (Source: [`skeleton_driver.cc`][skeleton-driver-cc])

   This driver constructor needs to pass the driver name (for example,
   `"skeleton_driver"`), `start_args`, and `driver_dispatcher` to the
   `DriverBase` class.

## Add the driver export macro {:#add-the-driver-export-macro .numbered}

To add the driver export macro, do the following:

1. In the driver source file, include the following header file:

   ```cpp
   #include <lib/driver/component/cpp/driver_export.h>
   ```

1. Add the following macro (which exports the driver class) at the
   bottom of the driver source file:

   ```cpp
   FUCHSIA_DRIVER_EXPORT(skeleton::SkeletonDriver);
   ```

   For example:

   ```cpp
   #include <lib/driver/component/cpp/driver_base.h>
   #include <lib/driver/component/cpp/driver_export.h>

   #include "skeleton_driver.h"

   namespace skeleton {

   SkeletonDriver::SkeletonDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
       : DriverBase("skeleton_driver", std::move(start_args),
             std::move(driver_dispatcher)) {
   }

   zx::result<> SkeletonDriver::Start() {
     return zx::ok();
   }

   }  // namespace skeleton

   FUCHSIA_DRIVER_EXPORT(skeleton::SkeletonDriver);
   ```

   (Source: [`skeleton_driver.cc`][skeleton-driver-cc])

## Create a build file {:#create-a-build-file .numbered}

To create a build file for the driver, do the following:

1. Create a new `BUILD.gn` file.
1. Include the following line to import the driver build rules:

   ```gn
   import("//build/drivers.gni")
   ```

1. Add a target for the driver, for example:

   ```gn
   fuchsia_driver("driver") {
     output_name = "skeleton_driver"
     sources = [ "skeleton_driver.cc" ]
     deps = [
       "//sdk/lib/driver/component/cpp",
       "//src/devices/lib/driver:driver_runtime",
     ]
   }
   ```

   (Source: [`BUILD.gn`][build-gn])

   The `output_name` field must be unique among all drivers.

## Write bind rules {:#write-bind-rules .numbered}

To write bind rules for your driver, do the following:

1. Create a new bind rule file (`.bind`) for the driver
   (for example, `skeleton_driver.bind`).

1. Add basic bind rules, for example:

   ```
   using gizmo.example;
   gizmo.example.TEST_NODE_ID == "skeleton_driver";
   ```

   (Source: [`skeleton_driver.bind`][skeleton-driver-bind])

1. In the `BUILD.gn` file, include the following line to import the
   bind build rules:

   ```gn
   import("//build/bind/bind.gni")
   ```

1. In the `BUILD.gn` file, add a target for the driver's bind rules,
   for example:

   ```gn
   driver_bind_rules("bind") {
     rules = "skeleton.bind"
     bind_output = "skeleton_driver.bindbc"
     deps = [ "//examples/drivers/bind_library:gizmo.example" ]
   }
   ```

   (Source: [`BUILD.gn`][build-gn])

   The `bind_output` field must be unique among all drivers.

   Note: To learn more about finding node properties and writing
   bind rules, see [Bind Rules Tutorial][node-properties].

## Create a driver component {:#create-a-driver-component .numbered}

To create a Fuchsia component for the driver, do the following:

1. Create a new component manifest file (`.cml`) in the `meta`
   directory (for example, `skeleton_driver.cml`).

1. Include the following component shards:

   ```
   {
       include: [
           "inspect/client.shard.cml",
           "syslog/client.shard.cml",
       ],
   }
   ```

1. Add the driver's `program` information using the following format:

   ```
   {
       program: {
           runner: "driver",
           binary: "driver/<OUTPUT_NAME>.so",
           bind: "meta/bind/<BIND_OUTPUT>",
       },
   }
   ```

   The `binary` field must match the `output_name` field in the
   `fuchsia_driver` target of the `BUILD.gn` file, and the `bind`
   field must match `bind_output` in the `driver_bind_rules` target,
   for example:

   ```
   {
       include: [
           "inspect/client.shard.cml",
           "syslog/client.shard.cml",
       ],
       program: {
           runner: "driver",
           binary: "driver/skeleton_driver.so",
           bind: "meta/bind/skeleton.bindbc",
       },
   }
   ```

   (Source: [`skeleton_driver.cml`][skeleton-driver-cml])

1. Create a new JSON file to provide the component's information
   (for example, `component-info.json`).

1. Add the driver component's information in JSON format, for example:

   ```json
   {
       "short_description": "Driver Framework example for a skeleton DFv2 driver",
       "manufacturer": "",
       "families": [],
       "models": [],
       "areas": [
           "DriverFramework"
       ]
   }
   ```

   (Source: [`component-info.json`][component-info-json])

1. In the `BUILD.gn` file, include the following line to import the component
   build rules:

   ```gn
   import("//build/components.gni")
   ```

1. In the `BUILD.gn` file, add a target for the driver component, for example:

   ```gn
   fuchsia_driver_component("component") {
     component_name = "skeleton"
     manifest = "meta/skeleton.cml"
     deps = [
        ":bind",
        ":driver"
     ]
     info = "component-info.json"
   }
   ```

   (Source: [`BUILD.gn`][build-gn])

   See the rules for these fields below:

   - Set the `manifest` field to the location of the driver's `.cml` file.
   - Set the `info` field to the location of the driver component
     information JSON file.
   - Set the `deps` array to include the `fuchsia_driver` and
     `driver_bind_rules` targets from the `BUILD.gn` file.

You can now build, load, and register this DFv2 driver in a Fuchsia system

## Additional tasks {:#additional-tasks}

Note: The instructions below are based on the [Simple driver][simple-driver].

This section provides additional features you can add to your minimal DFv2
driver:

- [Add logs](#add-logs)
- [Add a child node](#add-a-child-node)
- [Clean up the driver](#clean-up-the-driver)
- [Add a compat device server](#add-a-compat-device-server)

### Add logs {:#add-logs}

By default, to print logs from a DFv2 driver, use the `FDF_LOG` macro, for
example:

```cpp
FDF_LOG(INFO, "Starting SimpleDriver")
```

In addition to using the `FDF_LOG` macro, you can also print logs using
Fuchsia's structured logger library
([`structured_logger.h`][structured-logger-h]), which uses the
`FDF_SLOG` macro.

To use structured logs from your DFv2 driver, do the following:

1. Include the following header:

   ```cpp
   #include <lib/driver/logging/cpp/structured_logger.h>
   ```

1. Use the `FDF_SLOG` macro to print logs, for example:

   ```cpp
   FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
   ```

### Add a child node {:#add-a-child-node}

A DFv2 driver can add child nodes using the following `Node` protocol in the
[`fuchsia.driver.framework`][topology-fidl] FIDL library:

```none {:.devsite-disable-click-to-copy}
open protocol Node {
    flexible AddChild(resource struct {
        args NodeAddArgs;
        controller server_end:NodeController;
        node server_end:<Node, optional>;
    }) -> () error NodeError;
};
```

The driver can connect to the `Node` protocol using the `node()` value in the
`DriverStartArgs` object or from the `DriverBase` class's `node()` function.

In addition to using the `Node` protocol, you need to create `NodeController`
endpoints. The `AddChild()` method requires the server end of the
`fuchsia.driver.framework NodeController` protocol. On the other hand, the
driver is responsible for storing and keeping the client end of the protocol. If
this client end is deallocated, the driver framework removes the child node.

To add a child node using the `Node` protocol, do the following:

1. In your DFv2 driver, create an `NodeAddArgs` object, which takes the
   following arguments:

   ```none {:.devsite-disable-click-to-copy}
   type NodeAddArgs = resource table {
       /// Name of the node.
       1: name NodeName;
       2: offers vector<fuchsia.component.decl.Offer>:MAX_OFFER_COUNT;
       3: symbols vector<NodeSymbol>:MAX_SYMBOL_COUNT;
       4: properties NodePropertyVector;
       5: devfs_args DevfsAddArgs;
   };
   ```

   (Source: [`topology.fidl`][topology-fidl])

   - `name` is the name of the child node.
   - `offers` is the capabilities the parent is offering to the child node.
     You can create them with the `MakeOffer()` function in the
     [`node_adds_args`][node-adds-args] library.
   - `symbols` is the functions to be provided to the driver. It can be
     ignored for DFv2 drivers.
   - `properties` is the child node properties, which determines which
     driver becomes bound to the child node (for more information, see
     [Bind rules tutorial][bind-rules-tutorial]). You can create node
     properties with the `MakeProperty()` function in the
     [`node_adds_args`][node-adds-args] library.
   - `devfs_args` is required if the child node needs access to `devfs`.

   The example below creates an `NodeAddArgs` object:

   ```cpp
   fidl::Arena arena;
   auto properties = std::vector{fdf::MakeProperty(arena, bind_fuchsia_test::TEST_CHILD, "simple")};
     auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                     .name(arena, "simple_child")
                     .properties(arena, std::move(properties))
                     .Build();
   ```

1. Update the driver's `DriverBase` class to add a FIDL client object.

   The example below uses a `fidl::WireSyncClient` object:

   ```cpp
   class SimpleDriver : public fdf::DriverBase {
   <...>
   private:
   <...>
     fidl::WireSyncClient<fuchsia_driver_framework::NodeController> child_controller_;
   };
   ```

   This setup allows you to store the client end in the driver.

1. Create the server and client endpoints with the `fidl::CreateEndpoints`
   function, for example:

   ```cpp
   zx::result controller_endpoints =
       fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
   ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                 controller_endpoints.status_string());
   ```

1. Bind the client end to the FIDL client object, for example:

   ```cpp
   child_controller_.Bind(std::move(endpoints->client));
   ```

   You can now connect to the `Node` server using the `Node` object,

1. Connect to the `Node` server and call the `AddChild()` method, for example:

   ```cpp
    fidl::WireResult result =
         fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
   ```

   Putting all the steps together looks like the following example:

   ```cpp
   void SimpleDriver::Start(fdf::StartCompleter completer) {
     fidl::Arena arena;
     auto properties = std::vector{fdf::MakeProperty(arena, bind_fuchsia_test::TEST_CHILD, "skeleton")};
     auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                     .name(arena, "skeleton_child")
                     .properties(arena, std::move(properties))
                     .Build();

     zx::result controller_endpoints =
         fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
     ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create endpoints: %s",
                   controller_endpoints.status_string());
     controller_.Bind(std::move(controller_endpoints->client));

     fidl::WireResult result =
         fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
     if (!result.ok()) {
       FDF_LOG(ERROR, "Failed to add child %s", result.status_string());
       return completer(result.status());
     }

     completer(zx::ok());
   }
   ```

### Clean up the driver {:#clean-up-the-driver}

If a DFv2 driver needs to perform teardowns before it is stopped (for example,
stopping threads), then you need to override and implement additional
`DriverBase` methods: `PrepareStop()` and `Stop()`

The `PrepareStop()` function is called before the driver's `fdf` dispatchers are
shut down and the driver is deallocated. Therefore, the driver needs to
implement `PrepareStop()if` it needs to perform certain operations before the
driver's dispatchers shut down, for example:

```cpp
void SimpleDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
 // Teardown threads
  FDF_LOG(INFO, "Preparing to stop SimpleDriver");
  completer(zx::ok());
}
```

The `Stop()` function is called after all dispatchers belonging to this driver
are shut down, for example:

```cpp
void SimpleDriver::Stop() {
  FDF_LOG(INFO, "Stopping SimpleDriver");
}
```

### Add a compat device server {:#add-a-compat-device-server}

If your DFv2 driver has descendant DFv1 drivers that haven't yet migrated to
DFv2, you need to use the compatibility shim to enable your DFv2 driver to talk
to other DFv1 drivers in the system. For more details, see the
[Set up the compat device server in a DFv2 driver][set-up-compat-device-server]
guide.

<!-- Reference links -->

[skeleton-driver]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/
[simple-driver]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/simple/
[driver-base]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/component/cpp/driver_base.h
[skeleton-driver-h]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/skeleton_driver.h
[skeleton-driver-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/skeleton_driver.cc
[build-gn]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/BUILD.gn
[skeleton-driver-bind]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/skeleton_driver.bind
[node-properties]: /docs/development/drivers/tutorials/bind-rules-tutorial.md#looking_up_node_properties
[skeleton-driver-cml]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/meta/skeleton_driver.cml
[component-info-json]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/skeleton/component-info.json
[structured-logger-h]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/logging/cpp/structured_logger.h
[topology-fidl]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/topology.fidl
[node-adds-args]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/component/cpp/node_add_args.h
[bind-rules-tutorial]: /docs/development/drivers/tutorials/bind-rules-tutorial.md
[set-up-compat-device-server]: /docs/development/drivers/migration/set-up-compat-device-server.md
