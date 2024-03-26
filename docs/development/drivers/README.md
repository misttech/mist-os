# Drivers

Fuchsia’s driver framework is a collection of libraries, tools, metadata, and
components that enable developers to create, run, test, and distribute drivers for
Fuchsia systems. The driver framework aims to provide a stable ABI that allows
developers to write a driver once and deploy it on multiple versions of the Fuchsia
platform. (However, Fuchsia's driver framework is constantly evolving and has not
achieved ABI stability yet.)

The driver framework is composed of the driver manager, driver hosts, libraries,
FIDL interfaces, Banjo interfaces, and a set of guidelines for developing drivers
for Fuchsia:

- The **driver manager** is responsible for managing the life cycle of drivers,
  such as loading, unloading, and registering. It also provides ways for drivers
  to communicate with each other and with Fuchsia’s [Zircon][zircon] kernel.
- The **driver host** is a process that runs in the kernel and enables drivers to
  access kernel resources.
- The **driver runtime** is an in process library which facilitates communication
  and event handling.
- The **FIDL** interfaces are used for communication between drivers and the rest of
  the system.
- \[**DFv1 only**\] The **core library** (`libdriver`) provides a set of common functions
  that DFv1 drivers can use to interact with the driver manager and driver host.
- \[**DFv1 only**\] The **Banjo** interfaces are used for communication between drivers
  and the driver manager.

  (For more information on the differences between DFv1 and DFv2, see
  [Comparison between DFv1 and DFv2][dfv1-and-dfv2].)

For more details on these concepts for the new driver framework (DFv2), see
the [Drivers][dfv2-concepts] section under _Fundamentals_.

## Table of contents

### DFv1 to DFv2 driver migration

- [Overview][dfv1-to-dfv2-driver-migration-overview]
- [1. Migrate from DFv1 to DFv2][migrate-from-dfv1-to-dfv2]
- [2. Migrate from Banjo to FIDL][migrate-from-banjo-to-fidl]
- Extension

  - [Set up the compat device server in a DFv2 driver][set-up-compat-device-server]

### DFv2 driver development

- [Write a minimal DFv2 driver][write-a-minimal-driver]
- [Composite nodes][composite-nodes]
- [Driver stack performance][driver-stack-performance]
- [VMO Registration Pattern][vmo-registration-pattern]
- [DMA][dma]
- Tutorials

  - [Bind rules tutorial][bind-rules-tutorial]
  - [Bind library code generation tutorial][bind-library-code-generation-tutorial]
  - [FIDL tutorial][fidl-tutorial]

- Testing

  - [DriverTestRealm][driver-test-realm]
  - [Threading tips in tests][threading-tips-in-tests]

- Debugging

  - [Driver utilities][driver-utilities]

- [Driver runtime API guidelines][driver-runtime-api-guidelines]
- [Drivers rubric][drivers-rubric]

### DFv1 driver development

- [Fuchsia driver development (DFv1)][fuchsia-driver-development]
- [Building drivers][bulding-drivers]
- [Interrupts][interrupts]
- [Platform Bus][platform-bus]
- Tutorials

  - [Banjo tutorial][banjo-tutorial]
  - [Using the C++ DDK Template Library][using-cpp-ddk-template-lib]

- Testing

  - [Driver testing][driver-testing-overview]
  - [Mock DDK][mock-ddk]

- Debugging

  - [Using Inspect for drivers][using-inspect]
  - [Driver Logging][driver-logging]
  - [Add tracing to a driver][add-tracing]

### DFv1 concepts

- [Fuchsia Driver Framework (DFv1)][fuchsia-driver-framework]
- Device driver model

  - [Overview][device-driver-model-overview]
  - [Introduction][introduction]
  - [Device model][device-model]
  - [Driver binding][driver-binding]
  - [The Device ops][the-device-ops]
  - [Device driver lifecycle][device-driver-lifecycle]
  - [Device power management][device-power-management]
  - [Protocols in drivers][protocols-in-drivers]
  - [FIDL in drivers][fidl-in-drivers]
  - [Banjo in drivers][banjo-in-drivers]
  - [Composite devices][composite-devices]
  - [Device firmware][device-firmware]

### Driver-specific guides

-  Board drivers
   - [GPIO initialization][gpio-init]
-  Display drivers
   -  [How to write a display driver][how-to-write-a-display-driver]
   -  [Modifying board drivers][modifying-board-drivers]
   -  [What does a display controller do?][what-does-a-display-controller-do]
-  PCI drivers
   - [Configuration][configuration]
-  Registers
   -  [Registers overview][registers-overview]
-  USB drivers
   -  [Getting descriptors and endpoints from USB][getting-descriptors-and-endpoints-from-usb]
   -  [USB system overview][usb-system-overview]
   -  [Lifecycle of a USB request][lifecycle-of-a-usb-request]
   -  [USB mass storage driver][usb-mass-storage-driver]
-  Input drivers
   -  [Fuchsia input drivers][fuchsia-input-drivers]
   -  [Input report reader library][input-report-reader-library]
-  SDMMC drivers
   -  [SDMMC drivers architecture][sdmmc-drivers-architecture]

### Others

- [Deleting drivers][deleting-drivers]


<!-- Reference links -->

[dfv2-concepts]: /docs/concepts/drivers/README.md
[dfv2-development]: /docs/get-started/sdk/get-started-with-driver.md
[zircon]: /docs/concepts/kernel/README.md
[dfv1-and-dfv2]: /docs/concepts/drivers/comparison_between_dfv1_and_dfv2.md
[dfv1-to-dfv2-driver-migration-overview]: migration/README.md
[migrate-from-banjo-to-fidl]: migration/migrate-from-banjo-to-fidl/overview.md
[migrate-from-dfv1-to-dfv2]: migration/migrate-from-dfv1-to-dfv2/overview.md
[fuchsia-driver-development]: developer_guide/driver-development.md
[composite-nodes]: developer_guide/composite-node.md
[driver-runtime-api-guidelines]: developer_guide/driver-runtime-api-guidelines.md
[drivers-rubric]: developer_guide/rubric.md
[how-to-write-a-display-driver]: driver_guides/display/how_to_write.md
[modifying-board-drivers]: driver_guides/display/board_driver_changes.md
[what-does-a-display-controller-do]: driver_guides/display/hardware_concepts.md
[registers-overview]: driver_guides/registers/overview.md
[getting-descriptors-and-endpoints-from-usb]: driver_guides/usb/getting_descriptors_and_endpoints.md
[usb-system-overview]: driver_guides/usb/concepts/overview.md
[lifecycle-of-a-usb-request]: driver_guides/usb/concepts/request-lifecycle.md
[usb-mass-storage-driver]: driver_guides/usb/concepts/usb-mass-storage.md
[driver-testing-overview]: testing/overview.md
[mock-ddk]: testing/mock_ddk.md
[driver-test-realm]: testing/driver_test_realm.md
[threading-tips-in-tests]: testing/threading-tips-in-tests.md
[using-inspect]: diagnostics/inspect.md
[driver-logging]: diagnostics/logging.md
[add-tracing]: diagnostics/tracing.md
[driver-utilities]: diagnostics/driver-utils.md
[banjo-tutorial]: tutorials/banjo-tutorial.md
[bind-rules-tutorial]: tutorials/bind-rules-tutorial.md
[fidl-tutorial]: tutorials/fidl-tutorial.md
[bind-library-code-generation-tutorial]: tutorials/bind-libraries-codegen.md
[bulding-drivers]: best_practices/build.md
[deleting-drivers]: best_practices/deleting.md
[driver-stack-performance]: best_practices/driver_stack_performance.md
[vmo-registration-pattern]: best_practices/vmo-registration-pattern.md
[fuchsia-driver-framework]: concepts/fdf.md
[device-driver-model-overview]: concepts/device_driver_model/README.md
[introduction]: concepts/device_driver_model/introduction.md
[device-model]: concepts/device_driver_model/device-model.md
[driver-binding]: concepts/device_driver_model/driver-binding.md
[the-device-ops]: concepts/device_driver_model/device-ops.md
[device-driver-lifecycle]: concepts/device_driver_model/device-lifecycle.md
[device-power-management]: concepts/device_driver_model/device-power.md
[protocols-in-drivers]: concepts/device_driver_model/protocol.md
[platform-bus]: concepts/device_driver_model/platform-bus.md
[fidl-in-drivers]: concepts/device_driver_model/fidl.md
[banjo-in-drivers]: concepts/device_driver_model/banjo.md
[composite-devices]: concepts/device_driver_model/composite.md
[device-firmware]: concepts/device_driver_model/firmware.md
[driver-architectures-overview]: concepts/driver_architectures/README.md
[fuchsia-input-drivers]: concepts/driver_architectures/input_drivers/input.md
[input-report-reader-library]: concepts/driver_architectures/input_drivers/input_report_reader.md
[sdmmc-drivers-architecture]: concepts/driver_architectures/sdmmc_drivers/sdmmc.md
[using-cpp-ddk-template-lib]: concepts/driver_development/using-ddktl.md
[configuration]: concepts/driver_development/bar.md
[interrupts]: concepts/driver_development/interrupts.md
[dma]: concepts/driver_development/dma.md
[gpio-init]: concepts/driver_development/gpio-initialization.md
[set-up-compat-device-server]: migration/set-up-compat-device-server.md
[write-a-minimal-driver]: developer_guide/write-a-minimal-dfv2-driver.md
