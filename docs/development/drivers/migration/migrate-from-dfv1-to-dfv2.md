# Migrate from DFv1 to DFv2

Important: This phase assumes that your DFv1 driver has already been migrated
[from Banjo to FIDL][migrate-from-banjo-to-fidl].

When migrating a DFv1 driver to DFv2, a major part of work is to
[update the driver interfaces](#update-the-driver-interfaces-from-dfv1-to-dfv2)
to DFv2. However, if your target driver needs to talk to other DFv1 drivers
that haven't yet migrated to DFv2, you need to use the
[compatibility shim](#update-dependencies-for-the-compatibility-shim) to
enable your now-DFv2 driver to talk to other DFv1 drivers in the system.

## List of migration tasks {:#list-of-migration-tasks}

DFv1-to-DFv2 migration tasks are:

- [Update dependencies from DDK to DFv2](#update-dependencies-from-ddk-to-dfv2)
- [(Optional) Update dependencies for the compatibility shim](#update-dependencies-for-the-compatibility-shim)
- [Update the driver interfaces from DFv1 to DFv2](#update-the-driver-interfaces-from-dfv1-to-dfv2)
- [Use the DFv2 service discovery](#use-the-dfv2-service-discovery)
- [Update component manifests of other drivers](#update-component-manifests-of-other-drivers)
- [Expose a devfs node from the DFv2 driver](#expose-a-devfs-node-from-the-dfv2-driver)
- [Use dispatchers](#use-dispatchers)
- [Use the DFv2 inspect](#use-the-dfv2-inspect)
- [Use the DFv2 logger](#use-the-dfv2-logger)
- ([Optional) Implement your own load_firmware method](#implement-your-own-load-firmware-method)
- ([Optional) Use the node properties generated from FIDL service offers](#use-the-node-properties-generated-from-fidl-service-offers)
- [Update unit tests to DFv2](#update-unit-tests-to-dfv2)

For more information and examples, see
[Additional resources](#additional-resources).

## Before you start (Frequently asked questions) {:#before-you-start}

Before you start jumping into the DFv1-to-DFv2 migration tasks, the
frequently asked questions below can help you identify special conditions
or edge cases that may apply to your driver.

- **What is the compatibility shim and when is it necessary?**

  DFv1 and DFv2 drivers exist under a single branch of the node topology
  tree (that is, until all the drivers are migrated to DFv2) and they need
  to be able to talk to each other. To help with the migration process,
  Fuchsia's driver framework team created a compatibility shim to enable
  DFv1 drivers to live in DFv2.

  If your target driver talks to other DFv1 drivers that still use
  [Banjo][banjo] and those drivers won't be migrated to DFv2 all at once,
  you need to
  [use this compatibility shim](#update-dependencies-for-the-compatibility-shim)
  (by manually creating `compat::DeviceServer`) for enabling the drivers
  in different framework versions to talk to each other.

- **Can DFv2 drivers talk to Banjo protocols using the compatibility shim?**

  While it's strongly recommended that your DFv1 driver is migrated from
  Banjo to FIDL, if it is necessary for a DFv2 driver to talk
  to some existing Banjo protocols, the compatibility shim provides the
  following features:

  - `compat::BanjoServer` makes it easier to serve Banjo
    (see [`banjo_server.h`][banjo-server-h]).
  - `compat::ConnectBanjo` makes it easier to connect to Banjo
    (see [`banjo_client.h`][banjo-client-h]).

  Note: For more details on using the compatibility shim in a DFv2 driver
  to talk to its descendant DFv1 drivers, see the
  [Set up the compat device server in a DFv2 driver][set-up-compat-device-server]
  guide under the Extension section.

- **Can DFv2 drivers use the compatibility shim for composite nodes?**

  The migration process for composite drivers are nearly identical to
  normal drivers, but composite drivers have slightly different
  ways for connecting to Banjo or FIDL protocols from parent nodes.

  Because composite nodes have multiple parents, composite drivers need
  to identify the parent’s name when connecting to it. For example,
  below is a normal driver establishing a Banjo connection with its
  parent:

  ```cpp
  zx::result client_result =
      compat::ConnectBanjo<ddk::HidDeviceProtocolClient>(incoming());
  ```

  The composite driver’s method is almost identical, except the parent
  name needs to be added:

  ```cpp
  zx::result client_result =
      compat::ConnectBanjo<ddk::HidDeviceProtocolClient>(incoming(), "gpio-int")
  ```

- **What has changed in the new DFv2 driver interfaces?**

  One major change in DFv2 is that drivers take control of the life cycle
  of the child [nodes][driver-node] (or devices) created by the drivers.
  This is different from DFv1 where the driver framework manages the life
  cycles of devices, such as [tearing down devices][device-lifecycle],
  through the device tree.

  In DFv1, devices are controlled by [`zx_protocol_device`][ddk-device-h-77]
  while drivers are controlled by [`zx_driver_ops`][ddk-driver-h-29].
  If `ddktl` is used, the interfaces in `zx_protocol_device` need to be
  wrapped by `Ddk*()` functions in the mixin template class. In DFv2,
  [those interfaces](#update-the-driver-interfaces-from-dfv1-to-dfv2)
  have changed significantly.

- **How does service discovery work in DFv2?**

  In DFv2, using a FIDL service is required to establish a protocol
  connection. The parent driver adds a FIDL service to the
  `driver::OutgoingDirectory` object and serves it to the child node,
  which then enables the parent driver to offer the service to the
  child node.

  DFv1 and DFv2 drivers do this differently in the following ways:

  - In DFv1, the driver sets and passes the offer from the
    `DeviceAddArgs::set_runtime_service_offers()` call. Then the driver
    creates an `driver::OutgoingDirectory` object and passes the client
    end handle through the `DeviceAddArgs::set_outgoing_dir()` call.

  - In DFv2, the driver sets and passes the offer from the
    `NodeAddArgs::offers` object. The driver adds the service to the
    outgoing directory wrapped by the `DriverBase` class (originally
    provided by the `Start()` function). When the child driver binds to
    the child node, the driver host passes the incoming namespace
    containing the service to the child driver's `Start()` function.

  On the child driver side, DFv1 and DFv2 drivers also connect to the
  protocol providing the service in different ways:

  - A DFv1 driver calls the `DdkConnectRuntimeProtocol<ProtocolName>()`
    method.
  - A DFv2 driver calls the `driver::Connect<ProtocolName>()` method
    (or `context().incoming()->Connect<ProtocolName>()` if the
    `DriverBase` class is used).

  For more information, see
  [Use the DFv2 service discovery](#use-the-dfv2-service-discovery).

- **How does my driver's node (or device) get exposed in the system in DFv2?**

  Fuchsia has a global tree of devices exposed as a filesystem known as
  [`devfs`][devfs], which is routed to most components as `/dev`. When
  a driver adds a [device node][driver-node], it has the option of adding
  a "file" into `devfs`. Then this file in `devfs` allows other components
  in the system to talk to the driver. For instance, an audio driver may add
  a speaker device node and the audio driver wants to make sure that other
  components can use this node to output audio to the speaker. To accomplish
  this, the audio driver
  [adds (or exposes) a `devfs` node](#expose-a-devfs-node-from-the-dfv2-driver)
  for the speaker so that it appears as `/dev/class/audio/<random_number>`
  in the system.

- **What is not implemented in DFv2 that was available in DFv1?**

  If your DFv1 driver calls the [`load_firmware()`][load-firmware] function
  in the DDK library, you need to implement your own since an equivalent
  function is not available in DFv2. However, this is expected to be
  [simple to implement](#implement-your-own-load-firmware-method).

- **What has changed in the bind rules in DFv2?**

  DFv2 nodes contain additional
  [node properties generated from their FIDL service offers](#use-the-node-properties-generated-from-fidl-service-offers).

  However, it is unlikely that you will need to modify bind rules when
  migrating an existing DFv1 driver to DFv2.

- **What has changed in logging in DFv2?**

  DFv2 drivers cannot use the `zxlogf()` function or any debug library
  that wraps or uses this function. `zxlogf()` is defined in
  `//src/lib/ddk/include/lib/ddk/debug.h` and is removed from the
  dependencies in DFv2. Drivers migrating to DFv2 need to
  [stop using this library](#use-the-dfv2-logger) and other libraries
  that depend on it.

  However, a new [compatibility library][logging-h], which is only
  available in the Fuchsia source tree (`fuchsia.git`) environment, is
  now added to allow DFv2 drivers to use DFv1-style logging.

- **What has changed in inspect in DFv2?**

  DFv1 drivers use driver-specific inspect functions to create and update
  driver-maintained metrics. For instance, in DFv1 the
  `DeviceAddArgs::set_inspect_vmo()` function is called to indicate the
  VMO that the driver uses for inspect. In DFv2, however, we can just
  create an [`inspect::ComponentInspector`](#use-the-dfv2-inspect) object.

- **What do dispatchers do in DFv2?**

  A FIDL file generates templates and data types for a client-and-server
  pair. Between these client and server ends is a channel, and the
  dispatchers at each end fetch data from the channel. For more
  information on dispatchers, see
  [Driver dispatcher and threads][driver-dispatcher].

- **What are some issues with the new threading model when migrating
  a DFv1 driver to DFv2?**

  FIDL calls in DFv2 are not on a single thread basis and are asynchronous
  by design (although you can make them synchronous by adding `.sync()`
  to FIDL calls or using `fdf::WireSyncClient`). Drivers are generally
  discouraged from making synchronous calls because they can block other
  tasks from running. (However, if necessary, a driver can create a
  dispatcher with the `FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS` option,
  which is only supported for
  [synchronized dispatchers][synchronized-dispatchers].)

  Given the differences in the threading models between Banjo (DFv1) and
  FIDL (DFv2), you'll need to decide which kind of FIDL call (that is,
  synchronous or asynchronous) you want to use while migrating. If your
  original code is designed around the synchronous nature of Banjo and
  is hard to unwind to make it all asynchronous, then you may want to
  consider using the synchronous version of FIDL at first (which,
  however, may result in performance degradation for the time being).
  Later, you can revisit these calls and optimize them into using
  synchronous calls.

- **What has changed in testing drivers in DFv2?**

  The `mock_ddk` library, which is used in driver unit tests, is
  specific to DFv1. [New testing libraries](#update-unit-tests-to-dfv2)
  are now available for DFv2 drivers.

- **Should I fork my driver into a DFv2 version while working on migration?**

  Forking an existing driver for migration depends on the complexity
  of the driver. In general, it is recommended to avoid forking a
  driver because it could end up creating more work. However,
  for larger drivers, it may make sense to fork the driver into
  a DFv2 version so that you can gradually land migration changes
  in smaller patches.

  You can fork a driver by adding a new driver component in the GN args
  and use a flag to decide between the DFv1 or DFv2 version. This
  [example CL][gc-msd-arm-mali]{:.external} demonstrates how a DFv2 fork
  of the `msd-arm-mali` driver was added.

- **What are some recommended readings?**

  The [DFv2 concept docs][driver-concepts] on fuchsia.dev and this
  [Gerrit change][gc-intel-wifi]{:.external} from the previous DFv1
  Intel WiFi driver migration (the
  [`pcie-iwlwifi-driver.cc`][pcie-iwlwifi-driver-cc]{:.external} file
  contains most of the new APIs).

## Update dependencies from DDK to DFv2 {:#update-dependencies-from-ddk-to-dfv2}

DFv1 drivers use the DDK library (`//src/lib/ddk`). For DFv2 drivers,
you can safely remove all package dependencies under this DDK library
directory and replace them with the following new library, which
includes most of the essential utilities for DFv2 drivers:

```none
//sdk/lib/driver/component/cpp:cpp
```

In header files, include the following library for the new DFv2 driver
interfaces:

```cpp
#include <lib/driver/component/cpp/driver_base.h>
```

Note: After updating dependencies from DDK to DFv2, your driver won't
compile until you complete the next
[Update the driver interfaces from DFv1 to DFv2](#update-the-driver-interfaces-from-dfv1-to-dfv2)
section.

## (Optional) Update dependencies for the compatibility shim {:#update-dependencies-for-the-compatibility-shim}

**For DFv1 drivers that need to talk to DFv2 drivers**, you need the
following packages for the compatibility shim:

```none
//sdk/lib/driver/compat/cpp:cpp
//sdk/lib/driver/compat/cpp:symbols
```

In header files, you need the following libraries for the compatibility shim:

```cpp
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/compat/cpp/connect.h>
```

## Update the driver interfaces from DFv1 to DFv2 {:#update-the-driver-interfaces-from-dfv1-to-dfv2}

DFv2 provides a virtual class called [`DriverBase`][driver-base] that
wraps regular routines for a driver. For DFv2 drivers, inheriting
`DriverBase` in your new driver class is recommended, which makes the
interfaces much simpler.

For example, let's say you have the following class in your DFv1 driver:

```cpp {:.devsite-disable-click-to-copy}
class MyExampleDriver;

using MyExampleDeviceType = ddk::Device<MyExampleDriver, ddk::Initializable,
   ddk::Unbindable>;

class MyExampleDriver : public MyExampleDeviceType {
 public:
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
}
```

If you update the interface to inherit from `DriverBase`, the new class
may look like below:

```cpp {:.devsite-disable-click-to-copy}
class MyExampleDriver : public fdf::DriverBase {
  public:
    Driver(fdf::DriverStartArgs start_args,
        fdf::UnownedSynchronizedDispatcher driver_dispatcher);

    zx::result<> Start() override;
    void Stop() override;
}
```

In addition to starting and stopping the driver (as shown in the example
above), the `DriverBase` class provides the objects that enable the driver
to communicate with other components (and drivers). For instance,
instead of declaring and creating your own outgoing directory in DFv1
(as in the [Migrate from Banjo to FIDL][migrate-from-banjo-to-fidl] phase),
your driver can now call the `outgoing()` method of the `DriverBase` class
to retrieve an outgoing directory, for example:

```cpp {:.devsite-disable-click-to-copy}
class DriverBase {
   ...
   // Used to access the outgoing directory that the driver is serving. Can be used to add both
   // zircon and driver transport outgoing services.
   std::shared_ptr<OutgoingDirectory>& outgoing() { return outgoing_; }

   ...
}
```

(Source: [`driver_base.h`][driver-base])

Another useful class available in DFv2 is the `Node` class. The example
below shows a DFv2 driver's code that connects to the `Node` server and
uses the [`AddChild()`][addchild] function to add a child node:

```cpp
zx::result<> MyExampleDriver::Start() {
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node(std::move(node()));
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, “example_node”)
                    .Build();

  zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT(controller_endpoints.is_ok());

  auto result = node_->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }
  return zx::ok();
}
```

The [`NodeController`][nodecontroller] endpoint (`controller_endpoints`
in the example above) passed to the `AddChild()` function can be used to
control the child node. For instance, this endpoint can be used to remove
the child node from the node topology, request the driver framework to bind
the node to a specific driver, or receive a callback when the child node is
bound. The example below shows a DFv2 driver's code that removes the child
node during shutdown:

```cpp
void MyExampleDriver::Stop() {
  // controller_endpoints defined in the previous example.
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController>
       node_controller(controller_endpoints->client);

  auto status = node_controller->Remove();
  if (!status.ok()) {
    FDF_LOG(ERROR, "Could not remove child: %s", status.status_string());
  }
}
```

Moreover, in DFv2, the `OnBind()` event is defined in the `NodeController`
protocol, which is invoked by the child node from the server side. The
`DriverBase::PrepareStop()` function provides a chance to perform
de-initializations before `DriverBase::Stop()` is called.

The table below shows the mapping of the common driver and device interfaces
between DFv1 and DFv2:

| DFv1        | DFv2        |
| ----------- | ----------- |
| `zx_driver_ops::bind()` | [`DriverBase::Start()`][driver-base-99] |
| `zx_driver_ops::init()` | [`DriverBase::Start()`][driver-base-99] |
| `zx_driver_ops::release()` | [`DriverBase::Stop()`][driver-base-99] |
| `device_add()` </br></br> `DdkAdd()` | [`Node::AddChild()`][driver-base-99] |
| `device_add_composite()`</br></br>`DdkAddComposite()` | **None**. Add composite nodes using specifications. See [Composite Node][composite-node]. |
| `device_add_composite_node_spec()`</br></br>`DdkAddCompositeNodeSpec()` | [`CompositeNodeManager::AddSpec()`][composite-node-spec] |
| `zx_protocol_device::init()`</br></br>`DdkInit()` | **None**. In DFv2, a driver is in charge of the life cycle of the nodes (devices) it adds. |
| `zx_protocol_device::unbind()`</br></br>`DdkUnbind()` | **None**. In DFv2, a driver is in charge of the life cycle of the nodes (devices) it adds. |
| `zx_protocol_device::release()`</br></br>`DdkRelease()` | [`NodeController::Remove()`][nodecontroller-remove] |
| `zx_protocol_device::get_protocol()`</br></br>`device_get_protocol()` | **None**. These methods are based on Banjo protocols In DFv2, all communications are in FIDL. |
| `zx_protocol_device::service_connect()`</br></br>`device_service_connect()`</br></br>`DdkServiceConnect()` | **None**. This is an old-fashioned approach for drivers to establish FIDL connections with each other. For more information, see [Use the DFv2 service discovery](#use-the-dfv2-service-discovery). |
| `Device_connect_runtime_protocol()`</br></br>`DdkConnectRuntimeProtocol()` | **None**. These are newly added methods for service and protocol discovery in DFv1. For more information, see [Use the DFv2 service discovery](#use-the-dfv2-service-discovery). |

Also, aside from updating the interfaces, you need to change the macro
that populates your driver interface functions:

- From:

  ```none
  ZIRCON_DRIVER()
  ```

- To:

  ```none
  FUCHSIA_DRIVER_EXPORT()
  ```

## Use the DFv2 service discovery {:#use-the-dfv2-service-discovery}

When working on driver migration, you will likely encounter one or more
of the following three scenarios in which two drivers establish a FIDL
connection (in `child driver -> parent driver` format):

- **Scenario 1**: DFv2 driver -> DFv2 driver
- **Scenario 2**: DFv1 driver -> DFv2 driver
- **Scenario 3**: DFv2 driver -> DFv1 driver

**Scenario 1** is the standard case for DFv2 drivers (this
[example][runtime-protocol-test] shows the new DFv2 syntax). To update
your driver under this scenario, see the
[DFv2 driver to DFv2 driver](#dfv2-driver-to-dfv2-driver) section below.

**Scenario 2 and 3** are more complicated because the DFv1 driver is
wrapped in the compatibility shim in the DFv2 world. However,
the differences are:

- In **scenario 2**, this [Gerrit change][gc-scenario-2]{:.external} shows
  a method that exposes a service from the DFv2 parent to the DFv1 child.

- In **scenario 3**, the driver is connected to the
  `fuchsia_driver_compat::Service::Device` protocol provided by the
  compatibility shim of the parent driver, and the driver calls the
  `ConnectFidl()` method through this protocol to connect to the real
  protocol (for an example, see this
  [Gerrit change][pcie-iwlwifi-driver-cc]{:.external}).

To update your driver under **scenario 2 or 3**, see the
[DFv1 driver to DFv2 driver (with compatibility shim)](#dfv1-driver-to-dfv2-driver)
section below.

### DFv2 driver to DFv2 driver {:#dfv2-driver-to-dfv2-driver}

To enable other DFv2 drivers to discover your driver's service,
do the following:

1. Update your driver's `.fidl` file.

   The protocol discovery in DFv2 requires adding `service` fields for
   the driver's protocols, for example:

   ```none
   library fuchsia.example;

   @discoverable
   @transport("Driver")
   protocol MyProtocol {
       MyMethod() -> (struct {
           ...
       });
   };

   {{ '<strong>' }}service Service {
       my_protocol client_end:MyProtocol;
   };{{ '</strong>' }}
   ```

1. Update the child driver.

   DFv2 drivers can connect to protocols in the same way as FIDL services,
   for example:

   ```cpp
   driver::Connect<fuchsia_example::Service::MyProtocol>
   ```

   You also need to update the component manifest (`.cml`) file to use
   your driver runtime service, for example:

   ```none
   use: [
       { service: "fuchsia.example.Service" },
   ]
   ```

1. Update the parent driver.

   Your parent driver needs to create a `driver::OutgoingDirectory` object.
   You can use the `driver::OutgoingDirectory::Create()` method or the
   `driver::DriverBase` class. With the `driver::OutgoingDirectory` object,
   you must use services rather than protocols.

   Then you need to add the runtime service to your outgoing directory.
   The example below is a driver that inherits from the `driver::DriverBase`
   class:

   ```cpp
   zx::status<> Start() override {
     auto protocol = [this](
         fdf::ServerEnd<fuchsia_example::MyProtocol> server_end) mutable {
       fdf::BindServer(dispatcher()->get(), std::move(server_end), this);
     };

     fuchsia_example::Service::InstanceHandler handler(
          {.my_protocol = std::move(protocol)});

     auto status =
           context().outgoing().AddService<fuchsia_wlan_phyimpl::Service>(
               std::move(handler));
     if (status.is_error()) {
       return status.take_error();
     }

     auto result = outgoing_dir_.Serve(std::move(server_end));
     if (result.is_error()) {
       return result.take_error();
     }

     return zx::ok();
   }
   ```

   Update the child node's [`NodeAddArgs`][nodeaddargs] to include an offer
   for your runtime service, for example:

   ```cpp
   {{ '<strong>' }}auto offers =
       std::vector{fdf::MakeOffer2<fuchsia_example::Service>(arena, name)};{{ '</strong>' }}

   fidl::WireSyncClient<fuchsia_driver_framework::Node> node(std::move(node()));
     auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                       .name(arena, “example_node”)
                       {{ '<strong>' }}.offers2(offers){{ '</strong>' }}
                       .Build();

     zx::result controller_endpoints =
          fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
     ZX_ASSERT(controller_endpoints.is_ok());

     auto result = node_->AddChild(
         args, std::move(controller_endpoints->server), {});
   ```

   Similarly, update the parent driver's component manifest (`.cml`) file to
   offer your runtime service, for example:

   ```cpp
   capabilities: [
       { service: "fuchsia.example.Service" },
   ],

   expose: [
       {
           service: "fuchsia.example.Service",
           from: "self",
       },
   ],
   ```

### DFv1 driver to DFv2 driver (with compatibility shim) {:#dfv1-driver-to-dfv2-driver}

To enable other DFv1 drivers to discover your DFv2 driver's service,
do the following:

1. Update the DFv1 drivers.

   You need to update the component manifest (`.cml`) files of the DFv1
   drivers in the same way as mentioned in the
   [DFv2 driver to DFv2 driver](#dfv2-driver-to-dfv2-driver) section above,
   for example:

   - Child driver:

     ```none
     {
         include: [
             "//sdk/lib/driver_compat/compat.shard.cml",
             "inspect/client.shard.cml",
             "syslog/client.shard.cml",
         ],
         program: {
             runner: "driver",
             compat: "driver/child-driver-name.so",
             bind: "meta/bind/child-driver-name.bindbc",
             colocate: "true",
         },
         {{ '<strong>' }}use: [
             { service: "fuchsia.example.Service" },
         ],{{ '</strong>' }}
     }
     ```

   - Parent driver:

     ```none
     {
         include: [
             "//sdk/lib/driver_compat/compat.shard.cml",
             "inspect/client.shard.cml",
             "syslog/client.shard.cml",
         ],
         program: {
             runner: "driver",
             compat: "driver/parent-driver-name.so",
             bind: "meta/bind/parent-driver-name.bindbc",
         },
         {{ '<strong>' }}capabilities: [
             { service: "fuchsia.example.Service" },
         ],
         expose: [
             {
                 service: "fuchsia.example.Service",
                 from: "self",
             },
         ],{{ '</strong>' }}
     }
     ```

1. Update the DFv2 driver.

   The example below shows a method that exposes a service from the DFv2
   parent to the DFv1 child:

   ```cpp
     fit::result<fdf::NodeError> AddChild() {
       fidl::Arena arena;

       auto offer = fdf::MakeOffer2<ft::Service>(kChildName);

       // Set the properties of the node that a driver will bind to.
       auto property =
           fdf::MakeProperty(1 /*BIND_PROTOCOL */, bind_fuchsia_test::BIND_PROTOCOL_COMPAT_CHILD);

       auto args = fdf::NodeAddArgs{
         {
           .name = std::string(kChildName),
           .properties = std::vector{std::move(property)},
           .offers2 = std::vector{std::move(offer)},
         }
       };

       // Create endpoints of the `NodeController` for the node.
       auto endpoints = fidl::CreateEndpoints<fdf::NodeController>();
       if (endpoints.is_error()) {
         return fit::error(fdf::NodeError::kInternal);
       }

       auto add_result = node_.sync()->AddChild(fidl::ToWire(arena, std::move(args)),
                                                std::move(endpoints->server), {});
   ```

   (Source: [`root-driver.cc`][root-driver-cc])

## Update component manifests of other drivers {:#update-component-manifests-of-other-drivers}

To complete the migration of a DFv1 driver to DFv2, you not only need
to update the component manifest (`.cml`) file of your target driver,
but you may also need to update the component manifest files of some
other drivers that interact with your now-DFv2 driver.

Do the following:

1. Update the component manifests of leaf drivers (that is, without
   child drivers) with the changes below:

   - Remove `//sdk/lib/driver/compat/compat.shard.cml` from the
     `include` field.
   - Replace the `program.compat` field with `program.binary`.

2. Update the component manifests of other drivers that perform the
   following tasks:

   - Access kernel `args`.
   - Create composite devices.
   - Detect reboot, shutdown, or rebind calls.
   - Talk to other drivers using the Banjo protocol.
   - Access metadata from a parent driver or forward it.
   - Talk to a DFv1 driver that binds to a node added by your driver.

   For these drivers, update their component manifest with the changes
   below:

   - Copy some of the `use` capabilities from
     [`compat.shard.cml`][compat-shard-cml] to the component manifest,
     for example:

     ```none
     use: [
         {
             protocol: [
                 "fuchsia.boot.Arguments",
                 "fuchsia.boot.Items",
                 "fuchsia.device.manager.SystemStateTransition",
                 "fuchsia.driver.framework.CompositeNodeManager",
             ],
         },
         { service: "fuchsia.driver.compat.Service" },
     ],
     ```

   - Set the `program.runner` field to `driver`, for example:

     ```none
     program: {
         runner: "driver",
         binary: "driver/compat.so",
     },
     ```

## Expose a devfs node from the DFv2 driver {:#expose-a-devfs-node-from-the-dfv2-driver}

To expose a [`devfs`][devfs] node from a DFv2 driver, you need to add
the `device_args` member to the [`NodeAddArgs`][nodeaddargs].
In particular, it requires specifying the class name as well as
implementing the connector, which can be simplified by making use of
the [`Connector`][connector] library, for example:

```cpp
zx::result connector = devfs_connector_.Bind(dispatcher());
if (connector.is_error()) {
  return connector.take_error();
}

auto devfs =
    fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena).connector(
        std::move(connector.value()));

auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, name)
                    .devfs_args(devfs.Build())
                    .Build();
```

(Source: [`parent-driver.cc`][v2-parent-driver-cc])

For more information, see
[Expose the driver capabilities][codelab-driver-service] in the
DFv2 driver codelab. Also, see this [implementation][export-to-devfs]
of the `ExportToDevfs` method mentioned in the codelab.

## Use dispatchers {:#use-dispatchers}

[Dispatchers][driver-dispatcher] fetch data from a channel between
a FIDL client-and-server pair. By default, FIDL calls in this channel
are asynchronous.

For introducing asynchronization to drivers in DFv2, see the following
suggestions:

- The `fdf::Dispatcher::GetCurrent()` method gives you the default
  dispatcher that the driver is running on (see this
  [`aml-ethernet`][aml-ethernet] driver example). If possible, it is
  recommended to use this default dispatcher alone.

- Consider using multiple dispatchers for the following reasons
  (but not limited to):

  - The driver requires parallelism for performance.

  - The driver wants to perform blocking operations (because it is
    either a legacy driver or a non-Fuchsia driver being ported to
    Fuchsia) and it needs to handle more work while blocked.

- If multiple dispatchers are needed, the `fdf::Dispatcher::Create()`
  method can create new dispatchers for your driver. However, you must
  call this method on the default dispatcher (for example, call it
  inside the `Start()` hook) so that the driver host is aware of the
  additional dispatchers that belong to your driver.

- In DFv2, you don't need to shut down the dispatchers manually. They
  will be shut down between the `PrepareStop()` and `Stop()` calls.

For more details on migrating a driver to use multiple dispatchers,
see the
[Update the DFv1 driver to use non-default dispatchers][use-non-default-dispatchers]
section (in the [Migrate from Banjo to FIDL][migrate-from-banjo-to-fidl]
phrase).

## Use the DFv2 inspect {:#use-the-dfv2-inspect}

To set up driver-maintained [inspect][driver-inspect] metrics in DFv2,
you need to create an `inspect::ComponentInspector` object, for example:

```cpp
component_inspector_ =
    std::make_unique<inspect::ComponentInspector>(out, dispatcher, *inspector_);
```

(Source: [`driver-inspector.cc`][driver-inspector])

Creating an `inspect::ComponentInspector` object needs the following
three input items:

- The `component::OutgoingDirectory` object from the
  `Context().outgoing()->component()` call

- A dispatcher

- The original `inspect::Inspector` object

However, the DFv2 inspect does not require passing the VMO of
`inspect::Inspector` to the driver framework.

## Use the DFv2 logger {:#use-the-dfv2-logger}

Instead of using `zxlogf()` (which is deprecated in DFv2), the new
logging mechanism in DFv2 depends on the `driver::Logger` object,
which is passed from the driver host through `DriverStartArgs`
when starting the driver.

The `driver::DriverBase` class wraps `driver::Logger` and the driver
can get its reference by calling the `logger()` method (see this
[`wlantap-driver`][wlantap-driver] driver example). With this reference,
you can print out logs using the `logger.logf()` function or using these
[macros][logger-h], for example:

```cpp
FDF_LOG(INFO, "Example log message here");
```

## (Optional) Implement your own load_firmware method {:#implement-your-own-load-firmware-method}

If your DFv1 driver calls the [`load_firmware()`][load-firmware]
function in the DDK library, you need to implement your own version
of this function because an equivalent function is not available in DFv2.

This function is expected to be simple to implement. You need to get
the backing VMO from the path manually. For an example, see this
[Gerrit change][gc-pcie-iwlwifi-driver]{:.external}.

## (Optional) Use the node properties generated from FIDL service offers {:#use-the-node-properties-generated-from-fidl-service-offers}

DFv2 nodes contain the node properties generated from the FIDL service
offers from their parents.

For instance, in the [Parent Driver (The Server)][parent-driver-the-server]
example, the parent driver adds a node called `"parent"` with a service
offer for `fidl.examples.EchoService`. In DFv2, a driver that binds to this
node can have a bind rule for that FIDL service node property, for example:

```none
using fidl.examples.echo;

fidl.examples.echo.Echo == fidl.examples.echo.Echo.ZirconTransport;
```

For more information, see the
[Generated bind libraries][generate-bind-libraries] section of the FIDL
tutorial page.

## Update unit tests to DFv2 {:#update-unit-tests-to-dfv2}

The [`mock_ddk`][mock-ddk] library (which is used in unit tests for testing
driver and device life cycle) is specific to DFv1. The new DFv2 test
framework (see this [Gerrit change][gc-driver-testing]{:.external}) makes
mocked FIDL servers available to DFv2 drivers through the `TestEnvironment`
class.

The following libraries are available for unit testing DFv2 drivers:

- [`//sdk/lib/driver/testing/cpp`][driver-testing-cpp]

  - `TestNode` – This class implements the `fuchsia_driver_framework::Node`
    protocol, which can be provided to a driver to create child nodes. This
    class is also used by tests to access the child nodes that the driver
    has created.

  - `TestEnvironment` – A wrapper over an `OutgoingDirectory` object that
    serves as the backing VFS (virtual file system) for the incoming
    namespace of the driver under test.

  - `DriverUnderTest` – This class is a RAII
    ([Resource Acquisition Is Initialization][raii]{:.external}) wrapper
    for the driver under test.

  - `DriverRuntime` – This class is a RAII wrapper over the managed driver
    runtime thread pool.

- [`//sdk/lib/driver/testing/cpp/driver_runtime.h`][driver-testing-runtime]

  - `TestSynchronizedDispatcher` – This class is a RAII wrapper over the
    driver dispatcher.

The following library may be helpful for writing driver unit tests:

- [`//src/devices/bus/testing/fake-pdev/fake-pdev.h`][fake-pdev-h] – This
  helper library implements a fake version of the `pdev` FIDL protocol.

Lastly, the following example unit tests cover different configurations and
test cases:

- [`//sdk/lib/driver/component/cpp/tests/driver_base_test.cc`][driver-base-test-cc] -
  This file contains examples of the different threading models that driver
  tests can have.

- [`//sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc`][driver-fidl-test-cc] -
  This file demonstrates how to work with incoming and outgoing FIDL
  services fo both driver transport and Zircon transport as well as `devfs`.

## Additional resources {:#additional-resources}

Some DFv2 drivers examples:

- [`MagmaDriverBase`][magma-driver-base]
- [`WlanTapDriver`][wlantap-driver]
- [`AdcButtonsDriver`][adc-button-driver]

All the **Gerrit changes** mentioned in this section:

- [\[iwlwifi\] Dfv2 migration for iwlwifi driver][gc-iwlwifi-driver]{:.external}
- [\[compat-runtime-test\] Migrate off usage of DeviceServer][gc-scenario-2]{:.external}
- [\[msd-arm-mali\] Add DFv2 version][gc-msd-arm-mali-top-level]{:.external}
- [\[sdk\]\[driver\]\[testing\] Add testing library][gc-driver-testing]{:.external}

All the **source code files** mentioned in this section:

- [`//examples/drivers/transport/zircon/v2/parent-driver.cc`][v2-parent-driver-cc]
- [`//sdk/fidl/fuchsia.driver.framework/topology.fidl`][topology-fidl-80]
- [`//sdk/lib/driver/component/cpp/driver_base.h`][driver-base-h-70]
- [`//sdk/lib/driver/component/cpp/tests/driver_base_test.cc`][driver-base-test-cc]
- [`//sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc`][driver-fidl-test-cc]
- [`//sdk/lib/driver/compat/cpp/banjo_server.h`][banjo-server-h]
- [`//sdk/lib/driver/compat/cpp/banjo_client.h`][banjo-client-h]
- [`//sdk/lib/driver/compat/cpp/device_server.h`][device-server-h-23]
- [`//sdk/lib/driver/testing/cpp/driver_runtime.h`][driver-testing-runtime]
- [`//src/connectivity/wlan/testing/wlantap-driver/wlantap-driver.cc`][wlantap-driver]
- [`//src/devices/bus/testing/fake-pdev/fake-pdev.h`][fake-pdev-h]
- [`//src/devices/tests/v2/compat-runtime/root-driver.cc`][root-driver-cc]
- [`//src/lib/ddk/include/lib/ddk/device.h`][ddk-device-h-77]
- [`//src/lib/ddk/include/lib/ddk/driver.h`][load-firmware]
- [`//third_party/iwlwifi/platform/driver-inspector.cc`][driver-inspector]

All the **documentation pages** mentioned in this section:

- [Banjo][banjo]
- [Drivers and nodes][driver-node]
- [Driver communication][driver-communication]
- [Drivers and nodes][driver-node]
- [Driver dispatcher and threads][driver-dispatcher]
- [Drivers][driver-concepts]
- [Composite nodes][composite-node]
- [Expose the driver capabilities][codelab-driver-service]
- [Fuchsia component inspection overview][driver-inspect]
- [Mock DDK Migration][mock-ddk]
- [An Example of the Tear-Down Sequence][device-lifecycle]
  (from _Device driver lifecycle_)
- [Parent Driver (The Server)][parent-driver-the-server]
  (from _FIDL tutorial_)
- [Generated bind libraries][generate-bind-libraries]
  (from _FIDL tutorial_)

<!-- Reference links -->

[migrate-from-banjo-to-fidl]: /docs/development/drivers/migration/migrate-from-banjo-to-fidl.md
[banjo]: /docs/development/drivers/concepts/device_driver_model/banjo.md
[driver-dispatcher]: /docs/concepts/drivers/driver-dispatcher-and-threads.md
[driver-node]: /docs/concepts/drivers/drivers_and_nodes.md
[fake-pdev-h]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/devices/bus/testing/fake-pdev/fake-pdev.h
[driver-testing-cpp]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/testing/cpp/
[driver-base-test-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/component/cpp/tests/driver_base_test.cc
[device-lifecycle]: /docs/development/drivers/concepts/device_driver_model/device-lifecycle.md#an_example_of_the_tear-down_sequence
[ddk-device-h-77]: https://source.corp.google.com/fuchsia/src/lib/ddk/include/lib/ddk/device.h;l=77
[ddk-driver-h-29]: https://source.corp.google.com/fuchsia/src/lib/ddk/include/lib/ddk/driver.h;l=29
[load-firmware]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/lib/ddk/include/lib/ddk/driver.h;l=416
[logging-h]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/compat/cpp/logging.h
[synchronized-dispatchers]: /docs/concepts/drivers/driver-dispatcher-and-threads.md#synchronized-and-unsynchronized
[gc-intel-wifi]:https://fuchsia-review.git.corp.google.com/c/fuchsia/+/692243
[pcie-iwlwifi-driver-cc]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/692243/47/src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/pcie-iwlwifi-driver.cc
[driver-base]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/component/cpp/driver_base.h?q=sdk%2Flib%2Fdriver%2Fcomponent%2Fcpp%2Fdriver_base.h&ss=fuchsia%2Ffuchsia
[driver-base-99]: https://source.corp.google.com/h/turquoise-internal/turquoise/+/main:sdk/lib/driver/component/cpp/driver_base.h;l=99
[addchild]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/topology.fidl;drc=53130c6bb8b33ae921bb49a561966cbdbc2d6595;l=158
[nodecontroller]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/topology.fidl;l=101;drc=53130c6bb8b33ae921bb49a561966cbdbc2d6595
[runtime-protocol-test]: http://cs/fuchsia/src/devices/tests/v2/runtime-protocol/
[gc-scenario-2]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/920734
[pcie-iwlwifi-driver-cc]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/692243/47/src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/pcie-iwlwifi-driver.cc#323
[nodeaddargs]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/topology.fidl;l=81
[root-driver-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/devices/tests/v2/compat-runtime/root-driver.cc
[compat-shard-cml]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/compat/compat.shard.cml
[devfs]: /docs/concepts/drivers/driver_communication.md#service_discovery_using_devfs
[connector]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/devfs/cpp/connector.h;l=16
[v2-parent-driver-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/transport/zircon/v2/parent-driver.cc;l=93
[codelab-driver-service]: /docs/get-started/sdk/learn/driver/driver-service.md
[export-to-devfs]: https://fuchsia.googlesource.com/sdk-samples/drivers/+/refs/heads/main/src/qemu_edu/drivers/qemu_edu.cc#74
[use-non-default-dispatchers]: migrate-from-banjo-to-fidl.md#update-the-dfv1-driver-to-use-non-default-dispatchers
[aml-ethernet]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/connectivity/ethernet/drivers/aml-ethernet/aml-ethernet.cc;l=181
[driver-inspect]: /docs/development/drivers/diagnostics/inspect.md
[driver-inspector]: https://fuchsia.googlesource.com/drivers/wlan/intel/iwlwifi/+/refs/heads/main/third_party/iwlwifi/platform/driver-inspector.cc#25
[wlantap-driver]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/connectivity/wlan/testing/wlantap-driver/wlantap-driver.cc;l=61
[logger-h]: https://source.corp.google.com/h/turquoise-internal/turquoise/+/main:sdk/lib/driver/logging/cpp/logger.h;l=15
[load-firmware]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/lib/ddk/include/lib/ddk/driver.h;l=408
[gc-pcie-iwlwifi-driver]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/692243/47/src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/pcie-iwlwifi-driver.cc#60
[parent-driver-the-server]: /docs/development/drivers/tutorials/fidl-tutorial.md#parent_driver_the_server
[generate-bind-libraries]: /docs/development/drivers/tutorials/fidl-tutorial.md#generated-bind-libraries
[mock-ddk]: /docs/contribute/open_projects/graduated/mock_ddk_migration.md
[raii]: https://en.wikipedia.org/wiki/Resource_acquisition_is_initialization
[driver-testing-runtime]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/testing/cpp/driver_runtime.h
[driver-fidl-test-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc
[magma-driver-base]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/graphics/magma/lib/magma_service/sys_driver/magma_driver_base.h
[wlantap-driver]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/connectivity/wlan/testing/wlantap-driver/wlantap-driver.cc
[adc-button-driver]: https://cs.opensource.google/fuchsia/fuchsia/+/main:src/ui/input/drivers/adc-buttons/adc-buttons.h
[driver-base-h-70]: https://source.corp.google.com/fuchsia/sdk/lib/driver/component/cpp/driver_base.h;l=70?q=driverbase&sq=package:fuchsia
[topology-fidl-80]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/topology.fidl;l=80
[root-driver-cc]: https://source.corp.google.com/fuchsia/src/devices/tests/v2/compat-runtime/root-driver.cc
[device-server-h-23]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/compat/cpp/device_server.h;l=23?q=deviceserver&sq=&ss=fuchsia%2Ffuchsia
[driver-concepts]: /docs/concepts/drivers/README.md
[composite-node]: /docs/development/drivers/developer_guide/composite-node.md
[composite-node-spec]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/composite_node_spec.fidl;l=68
[nodecontroller-remove]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/fidl/fuchsia.driver.framework/topology.fidl;drc=53130c6bb8b33ae921bb49a561966cbdbc2d6595;l=103
[gc-iwlwifi-driver]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/692243
[gc-msd-arm-mali]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/853637/5/src/graphics/drivers/msd-arm-mali/BUILD.gn
[gc-msd-arm-mali-top-level]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/853637
[gc-driver-testing]: https://fuchsia-review.git.corp.google.com/c/fuchsia/+/770412
[driver-communication]: /docs/concepts/drivers/driver_communication.md
[banjo-server-h]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/compat/cpp/banjo_server.h
[banjo-client-h]: https://cs.opensource.google/fuchsia/fuchsia/+/main:sdk/lib/driver/compat/cpp/banjo_client.h
[set-up-compat-device-server]: set-up-compat-device-server.md
