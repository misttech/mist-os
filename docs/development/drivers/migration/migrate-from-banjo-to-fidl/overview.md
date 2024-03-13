# Migrate from Banjo to FIDL

DFv1 drivers communicate with each other using the [Banjo][banjo] protocol.
In DFv2, all communications occur over [FIDL][fidl] (Fuchsia Interface
Definition Language) calls, for both drivers and non-drivers. So if your
target driver for [DFv1-to-DFv2 migration][migrate-from-dfv1-to-dfv2] uses
the Banjo protocol, you'll also need to migrate the driver to use FIDL to
complete the migration.

Driver migration from Banjo to FIDL can be summarized as follows:

1. Update the driver's`.fidl` file to create a new FIDL interface.
2. Update the driver's code to use the new interface.
3. Build and test the driver using the new FIDL interface.

## Before you start {:#before-you-start}

Before you start jumping into the Banjo-to-FIDL migration tasks, check out
the [frequently asked questions][faq] page, which can help you identify
special conditions or edge cases that may apply to your driver.

## List of migration tasks {:#list-of-migration-tasks}

Banjo-to-FIDL migration tasks are:

- [Update the DFv1 driver from Banjo to FIDL][update-banjo-to-fidl]
- ([Optional) Update the DFv1 driver to use the driver runtime][update-driver-runtime]
- ([Optional) Update the DFv1 driver to use non-default dispatchers][update-non-default-dispatchers]
- ([Optional) Update the DFv1 driver to use two-way communication][update-two-way-communication]
- [Update the DFv1 driver's unit tests to use FIDL][update-unit-tests]

For more information and examples, see
[Additional resources][additional-resources].

<!-- Reference links -->

[banjo]: /docs/development/drivers/concepts/device_driver_model/banjo.md
[fidl]: /docs/concepts/fidl/overview.md
[migrate-from-dfv1-to-dfv2]: /docs/development/drivers/migration/migrate-from-dfv1-to-dfv2/overview.md
[faq]: /docs/development/drivers/migration/migrate-from-banjo-to-fidl/faq.md
[update-banjo-to-fidl]: convert-banjo-protocols-to-fidl-protocols.md#update-the-dfv1-driver-from-banjo-to-fidl
[update-driver-runtime]: convert-banjo-protocols-to-fidl-protocols.md#update-the-dfv1-driver-to-use-the-driver-runtime
[update-non-default-dispatchers]: convert-banjo-protocols-to-fidl-protocols.md#update-the-dfv1-driver-to-use-non-default-dispatchers
[update-two-way-communication]: convert-banjo-protocols-to-fidl-protocols.md#update-the-dfv1-driver-to-use-two-way-communication
[update-unit-tests]: convert-banjo-protocols-to-fidl-protocols.md#update-the-dfv1-drivers-unit-tests-to-use-fidl
[additional-resources]: convert-banjo-protocols-to-fidl-protocols.md#additional-resources

