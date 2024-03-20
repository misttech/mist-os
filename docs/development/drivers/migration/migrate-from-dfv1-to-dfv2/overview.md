# Migrate from DFv1 to DFv2

A major part of DFv1-to-DFv2 migration work involves updating the
[driver interfaces][driver-interfaces] to DFv2.

However, if your target driver has descendant DFv1 drivers that haven't yet
migrated to DFv2, you need to use the compatibility shim to enable your
now-DFv2 driver to talk to other DFv1 drivers in the system. For more
information on setting up and using this compatibility shim in a DFv2 driver,
see the
[Set up the compat device server in a DFv2 driver][set-up-compat-device-server]
guide.

## Before you start {:#before-you-start}

Before you start jumping into the DFv1-to-DFv2 migration tasks, check out
the [frequently asked questions][faq] page, which can help you identify special
conditions or edge cases that may apply to your driver.

## List of migration tasks {:#list-of-migration-tasks}

DFv1-to-DFv2 migration tasks are:

- [Update dependencies from DDK to DFv2][update-dependencies]
- [Update the driver interfaces from DFv1 to DFv2][update-driver-interfaces]
- [Use the DFv2 service discovery][use-service-discovery]
- [Update component manifests of other drivers][update-component-manifests]
- [Expose a devfs node from the DFv2 driver][expose-devfs]
- [Use dispatchers][use-dispatchers]
- [Use the DFv2 inspect][use-dfv2-inspect]
- ([Optional) Implement your own load_firmware method][implement-firmware]
- ([Optional) Use the node properties generated from FIDL service offers][use-node-properties]
- [Update unit tests to DFv2][update-unit-tests]

For more information and examples, see
[Additional resources][additional-resources].

<!-- Reference links -->

[faq]: faq.md
[driver-interfaces]: update-driver-interfaces-to-dfv2.md#update-the-driver-interfaces-from-dfv1-to-dfv2
[update-dependencies]: update-driver-interfaces-to-dfv2.md#update-dependencies-from-ddk-to-dfv2
[update-driver-interfaces]: update-driver-interfaces-to-dfv2.md#update-the-driver-interfaces-from-dfv1-to-dfv2
[use-service-discovery]: update-driver-interfaces-to-dfv2.md#use-the-dfv2-service-discovery
[update-component-manifests]: update-driver-interfaces-to-dfv2.md#update-component-manifests-of-other-drivers
[expose-devfs]: update-driver-interfaces-to-dfv2.md#expose-a-devfs-node-from-the-dfv2-driver
[use-dispatchers]: update-driver-interfaces-to-dfv2.md#use-dispatchers
[use-dfv2-inspect]: update-driver-interfaces-to-dfv2.md#use-the-dfv2-inspect
[implement-firmware]: update-driver-interfaces-to-dfv2.md#implement-your-own-load-firmware-method
[use-node-properties]: update-driver-interfaces-to-dfv2.md#use-the-node-properties-generated-from-fidl-service-offers
[update-unit-tests]: update-driver-interfaces-to-dfv2.md#update-unit-tests-to-dfv2
[additional-resources]: update-driver-interfaces-to-dfv2.md#additional-resources
[set-up-compat-device-server]: /docs/development/drivers/migration/set-up-compat-device-server.md
