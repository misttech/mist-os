# DFv1 to DFv2 driver migration

This playbook offers guidelines, best practices, examples, and reference
materials to help you migrate existing DFv1-based drivers, which are
stored in the Fuchsia source tree (`fuchsia.git`), to Fuchsia's
new [driver framework][driver-framework] (DFv2).

## Before you start {:#before-you-start}

DFv2 enables Fuchsia drivers to be fully user-space
[components][components]. Like any other Fuchsia component, a DFv2 driver
exposes and receives [FIDL][fidl] capabilities to and from other components
and drivers in the system.

Notice the following key differences between DFv1 and DFv2:

- **DFv1**: Drivers are not components. The [Banjo][banjo] protocol is
  used for driver-to-driver communication. Driver host interfaces or the
  DDK (Driver Development Kits) wrapper are used to manage the life cycle
  of drivers.

- **DFv2**: Drivers are components. FIDL is used for all communication,
  including communication between drivers and non-drivers. The driver
  framework manages the life cycle of drivers. (For more information,
  see [Comparison between DFv1 and DFv2][dfv1-vs-dfv2].)

Here is a list for the expected conditions of your driver after completing
the  migration to DFv2:

- The driver can be registered with the [driver manager][driver-manager].
- The driver can bind to a [device node][driver-node] in the system.
- Fuchsia components and drivers can use the driver's capabilities.
- Fuchsia devices can be flashed with product images containing the driver.
- All unit tests and integration tests for the driver are passed.

Before you begin working on driver migration, familiarize yourself with
the driver's unit tests and integration tests.

## Two phases in driver migration {:#two-phases-in-driver-migration}

When you're ready to migrate your DFv1 driver to DFv2, this playbook can
assist you with migration tasks in a linear manner. However, keep in mind
that, depending on your driver's features or settings, you may need to
handle additional tasks that aren't covered in this playbook.

Driver migration from DFv1 to DFv2 can be divided into two phases:

1. [Migrate from DFv1 to DFv2][migrate-from-dfv1-to-dfv2].
2. [Migrate from Banjo to FIDL][migrate-from-banjo-to-fidl].

### Extensions

The following guides are added to support tasks that were previously
identified as missing in the migration playbook above:

- [Set up the compat device server in a DFv2 driver][set-up-compat-device-server]
- [Serve Banjo protocols in a DFv2 driver][serve-banjo-protocols]

<!-- Reference links -->

[driver-framework]: /docs/concepts/drivers/driver_framework.md
[components]: /docs/concepts/components/v2/README.md
[banjo]: /docs/development/drivers/concepts/device_driver_model/banjo.md
[fidl]: /docs/concepts/fidl/overview.md
[dfv1-vs-dfv2]: /docs/concepts/drivers/comparison_between_dfv1_and_dfv2.md
[driver-manager]: /docs/concepts/drivers/driver_framework.md#driver_manager
[driver-node]: /docs/concepts/drivers/drivers_and_nodes.md
[migrate-from-banjo-to-fidl]: /docs/development/drivers/migration/migrate-from-banjo-to-fidl/overview.md
[migrate-from-dfv1-to-dfv2]: /docs/development/drivers/migration/migrate-from-dfv1-to-dfv2/overview.md
[set-up-compat-device-server]: /docs/development/drivers/migration/set-up-compat-device-server.md
[serve-banjo-protocols]: /docs/development/drivers/migration/serve-banjo-protocols.md
