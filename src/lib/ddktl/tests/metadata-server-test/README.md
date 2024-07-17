# Metadata Test
The goal of `:metadata-server-test` is to make sure that drivers can send and
retrieve metadata to each other using the `ddk::MetadataServer` class and
`ddk::GetMetadata()` function. The test will spawn a driver test realm and
include the following drivers: `metadata_sender`, `metadata_forwarder`, and
`metadata_receiver`.

Here is the device tree of the test realm created by `:metadata-server-test`:
```
[dev] pid=101984 fuchsia-boot:///dtr#meta/metadata_sender_test_driver.cm
  [metadata_sender] pid=102475 fuchsia-boot:///dtr#meta/metadata_forwarder_test_driver.cm
    [metadata_forwarder] pid=103327 fuchsia-boot:///dtr#meta/metadata_retriever_test_driver.cm
      [metadata_retriever] pid=None unbound
```

## Drivers
### metadata_sender
The `metadata_sender` driver's purpose is to serve metadata to its child
device using `ddk::MetadataServer`. The `metadata_forwarder` driver will bind to
`metadata_sender`'s child device.

### metadata_forwarder
The `metadata_forwarder` driver's purpose is to bind to
`metadata_sender`'s child device, retrieve metadata from `metadata_sender`, and
forward the metadata to its child device using
`ddk::MetadataServer::ForwardMetadata()`. The `metadata_retriever` will bind to `metadata_forwarder`'s child device.

### metadata_retriever
The `metadata_retriever` driver's purpose is to bind to `metadata_forwarder`
child device and retrieve metadata `metadata_forwarder` using
`ddk::GetMetadata()`.
