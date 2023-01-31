abr-shim
========

The abr-shim driver uses `//src/firmware/lib/abr` to reboot to recovery on
devices that don't support PSCI reset arguments. It does this by binding to the
GPT partition that contains ABR metadata based on the type GUID and setting the
one-shot recovery flag during the driver suspend hook. abr-shim implements
`fuchsia.hardware.block.BlockImpl` and
`fuchsia.hardware.block.partition.BlockPartition` and
forwards calls from children to its parent. Block ops received during the
suspend hook are completed with `ZX_ERR_CANCELED`.

Boards that need to use abr-shim should pass
`fuchsia.hardware.gpt.metadata.GptInfo` with `block_driver_should_ignore_device`
set for the ABR partition as metadata to the storage driver with
`DEVICE_METADATA_GPT_INFO`.
