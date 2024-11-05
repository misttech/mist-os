# Passing metadata between drivers tutorial

This guide explains how to pass metadata from one driver to another using the
[metadata](/sdk/lib/driver/metadata) library. Metadata is any arbitrary data
created by a driver when it is bound to a node which should be accessible to
drivers bound to its child nodes. Normally, drivers can create their own FIDL
protocol in order to pass metadata, however, the
[metadata](/sdk/lib/driver/metadata) library offers functionality to pass
metadata between drivers with less code.

You can find an example of drivers sending, retrieving and forwarding metadata
[here](/examples/drivers/metadata).

## Metadata definition
First, the type of metadata must be defined as a FIDL type:

```
library fuchsia.examples.metadata;

// Type of the metadata to be passed.
type Metadata = table {
    1: test_property string:MAX;
};
```

This metadata will be served in the driver's outgoing namespace using the
[fuchsia.driver.metadata/Service](/sdk/fidl/fuchsia.driver.metadata/fuchsia.driver.metadata.fidl)
FIDL service. However, the name of the service within the outgoing namespace
will not be `fuchsia.driver.metadata.Service`. Instead, it will be a custom
string that we will provide which is associated with the type of the metadata.
For this tutorial, we will define that string in the same FIDL library as above:

```
library fuchsia.examples.metadata;

const SERVICE string = "fuchsia.examples.metadata.Metadata";
```

The FIDL library's build target will be defined as the following:

```
fidl("fuchsia.examples.metadata") {
  sources = [ "fuchsia.examples.metadata.fidl" ]
}
```

## Metadata library
In order to both send and receive metadata using the
[metadata](/sdk/lib/driver/metadata) library, we need to specialize the
[fdf_metadata::ObjectDetails](/sdk/lib/driver/metadata/cpp/metadata.h) template
class like so:

```cpp
// Defines `fdf_metadata::ObjectDetails`.
#include <lib/driver/metadata/cpp/metadata.h>

// Defines the fuchsia.examples.metadata types in C++.
#include <fidl/fuchsia.examples.metadata/cpp/fidl.h>

// `ObjectDetails` must be specialized within the `fdf_metadata` namespace.
namespace fdf_metadata {

template <>
// Pass the metadata type as the template argument.
struct ObjectDetails<fuchsia_examples_metadata::Metadata> {

  // Specify the name of the service used to serve the metadata.
  inline static const char* Name = fuchsia_examples_metadata::kService;
};

}  // namespace fdf_metadata

```

Since both the sending and receiving drivers need to do this, we can insert this
code into a new library that will be included by both drivers:

```
source_set("examples_metadata") {
  # This header file should contain the above code.
  sources = [ "examples_metadata.h" ]

  public_deps = [
    # This should be the fuchsia.examples.metadata FIDL library's C++ target.
    ":fuchsia.examples.metadata_cpp",
  ]
}
```

## Sending metadata
### Initial setup
Let's say we have a driver that wants to send metadata to its children:

```cpp
#include <lib/driver/component/cpp/driver_export.h>

class Sender : public fdf::DriverBase {
 public:
  Sender(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("parent", std::move(start_args), std::move(driver_dispatcher)) {}
};

FUCHSIA_DRIVER_EXPORT(Sender);
```

It's component manifest is the following:

```
{
    include: [
        "inspect/client.shard.cml",
        "syslog/client.shard.cml",
    ],
    program: {
        runner: "driver",
        binary: "driver/parent.so",
        bind: "meta/bind/parent.bindbc",
    },
}
```

It's build targets are defined as follows:

```
fuchsia_cc_driver("driver") {
  testonly = true
  output_name = "parent"
  sources = [
    "parent.cc",
  ]
  deps = [
    "//src/devices/lib/driver:driver_runtime",
  ]
}

fuchsia_driver_component("component") {
  testonly = true
  component_name = "parent"
  manifest = "meta/parent.cml"
  deps = [
    ":driver",
    ":bind", # Bind rules not specified in this tutorial.
  ]
  info = "parent.json" # Info not specified in this tutorial.
}
```

### Send process
In order for this driver to send metadata to its child drivers, it will need an
instance of the
[fdf_metadata::MetadataServer](/sdk/lib/driver/metadata/cpp/metadata_server.h)
class, set the metadata by calling its
`fdf_metadata::MetadataServer::SetMetadata()` method, and then serve the
metadata to the driver's outgoing directory by calling its
`fdf_metadata::MetadataServer::Serve()` method:

```cpp
{% verbatim %}
// Make sure to include the metadata library that specializes the
// `fdf_metadata::ObjectDetails` class. It is needed by
// `fdf_metadata::MetadataServer`.
#include "examples_metadata.h"

#include <lib/driver/metadata/cpp/metadata_server.h>

class Sender : public fdf::DriverBase {
 public:
  zx::result<> Start() override {
    // Set the metadata to be served.
    fuchsia_examples_metadata::Metadata metadata{{.test_property = "test value"}};
    ZX_ASSERT(metadata_server_.SetMetadata(std::move(metadata)) == ZX_OK);

    // Serve the metadata to the driver's outgoing directory.
    ZX_ASSERT(metadata_server_.Serve(*outgoing(), dispatcher()) == ZX_OK);

    return zx::ok();
  }

 private:
  // Responsible for serving metadata.
  fdf_metadata::MetadataServer<fuchsia_examples_metadata::Metadata> metadata_server_;
};
{% endverbatim %}
```

`fdf_metadata::MetadataServer::SetMetadata()` can be called multiple times,
before or after `fdf_metadata::MetadataServer::Serve()`, and the metadata server
will serve the latest metadata instance. A driver will fail to retrieve the
metadata if `fdf_metadata::MetadataServer::SetMetadata()` has not been called
before it attempts to retrieve the metadata.

The `driver` build target will need to be updated:
```
fuchsia_cc_driver("driver") {
  testonly = true
  output_name = "parent"
  sources = [
    "parent.cc",
  ]
  deps = [
    "//src/devices/lib/driver:driver_runtime",
    ":examples_metadata",
  ]
}
```

### Exposing the metadata service
Finally, the driver needs to declare and expose the
`fuchsia.examples.metadata.Metadata` FIDL service in its component manifest:

```
{
    include: [
        "inspect/client.shard.cml",
        "syslog/client.shard.cml",
    ],
    program: {
        runner: "driver",
        binary: "driver/parent.so",
        bind: "meta/bind/parent.bindbc",
    },
    capabilities: [
        { service: "fuchsia.examples.metadata.Metadata" },
    ],
    expose: [
        {
            service: "fuchsia.examples.metadata.Metadata",
            from: "self",
        },
    ],
}
```

Technically, there is no `fuchsia.examples.metadata.Metadata` FIDL service.
Under the hood,
[fdf_metadata::MetadataServer](/sdk/lib/driver/metadata/cpp/metadata_server.h)
serves the
[fuchsia.driver.metadata/Service](/sdk/fidl/fuchsia.driver.metadata/fuchsia.driver.metadata.fidl)
FIDL service in order to send metadata. However,
[fdf_metadata::MetadataServer](/sdk/lib/driver/metadata/cpp/metadata_server.h)
does not serve this service under the name `fuchsia.driver.metadata.Service`
like a normal FIDL service would. Instead, the service is served under the name
`fuchsia.examples.metadata.Metadata`. This allows the receiving driver to
identify which type of metadata is being passed. This means we need to declare
and expose the `fuchsia.examples.metadata.Metadata` service even though that
service doesn't exist.

## Retrieving metadata
### Initial setup
Let's say we have a driver that wants to receive metadata from its parent
driver:

```cpp
#include <lib/driver/component/cpp/driver_export.h>

class Retriever : public fdf::DriverBase {
 public:
  Retriever(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("child", std::move(start_args), std::move(driver_dispatcher)) {}
};

FUCHSIA_DRIVER_EXPORT(Retriever);
```

It's component manifest is the following:

```
{
    include: [
        "inspect/client.shard.cml",
        "syslog/client.shard.cml",
    ],
    program: {
        runner: "driver",
        binary: "driver/child.so",
        bind: "meta/bind/child.bindbc",
    },
}
```

It's build targets are defined as follows:

```
fuchsia_cc_driver("driver") {
  testonly = true
  output_name = "child"
  sources = [
    "child.cc",
  ]
  deps = [
    "//src/devices/lib/driver:driver_runtime",
  ]
}

fuchsia_driver_component("component") {
  testonly = true
  component_name = "child"
  manifest = "meta/child.cml"
  deps = [
    ":driver",
    ":bind", # Bind rules not specified in this tutorial.
  ]
  info = "child.json" # Info not specified in this tutorial.
}
```

### Retrieval process
In order to retrieve the metadata from a driver's parent driver, the driver will
call `fdf_metadata::GetMetadata()`:

```cpp
#include <lib/driver/metadata/cpp/metadata.h>

// Make sure to include the metadata library that specializes the
// `fdf_metadata::ObjectDetails` class. It is needed by
// `fdf_metadata::GetMetadata()`.
#include "examples_metadata.h"

class Retriever : public fdf::DriverBase {
 public:
  zx::result<> Start() override {
    zx::result<fuchsia_examples_metadata::Metadata> metadata =
      fdf_metadata::GetMetadata<fuchsia_examples_metadata::Metadata>(incoming());
    ZX_ASSERT(!metadata.is_error());

    return zx::ok();
  }
};
```

The `driver` build target will need to be updated:

```
fuchsia_cc_driver("driver") {
  testonly = true
  output_name = "child"
  sources = [
    "child.cc",
  ]
  deps = [
    "//src/devices/lib/driver:driver_runtime",
    ":examples_metadata",
  ]
}
```

### Using the metadata service
Finally, the driver will need to declare its usage of the
`fuchsia.examples.metadata.Metadata` FIDL service in its component manifest:

```
{
    include: [
        "inspect/client.shard.cml",
        "syslog/client.shard.cml",
    ],
    program: {
        runner: "driver",
        binary: "driver/child.so",
        bind: "meta/bind/child.bindbc",
    },
    use: [
        { service: "fuchsia.examples.metadata.Metadata" },
    ],
}
```

## Forwarding metadata
### Initial setup
Let's say we have a driver that wants to retrieve metadata from its parent
driver and forward that metadata to its child drivers:

```cpp
#include <lib/driver/component/cpp/driver_export.h>

class Forwarder : public fdf::DriverBase {
 public:
  Forwarder(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("forward", std::move(start_args), std::move(driver_dispatcher)) {}
};

FUCHSIA_DRIVER_EXPORT(Forwarder);
```

It's component manifest is the following:

```
{
    include: [
        "inspect/client.shard.cml",
        "syslog/client.shard.cml",
    ],
    program: {
        runner: "driver",
        binary: "driver/forward_driver.so",
        bind: "meta/bind/forward.bindbc",
    },
}
```

It's build targets are defined as follows:

```
fuchsia_cc_driver("driver") {
  testonly = true
  output_name = "forward_driver"
  sources = [
    "forward_driver.cc",
  ]
  deps = [
    "//src/devices/lib/driver:driver_runtime",
  ]
}

fuchsia_driver_component("component") {
  testonly = true
  component_name = "forward"
  manifest = "meta/forward.cml"
  deps = [
    ":driver",
    ":bind", # Bind rules not specified in this tutorial.
  ]
  info = "forward.json" # Info not specified in this tutorial.
}
```

### Forward process
In order for the driver to forward metadata from its parent driver to its child
drivers, it will need an instance of the
[fdf_metadata::MetadataServer](/sdk/lib/driver/metadata/cpp/metadata_server.h)
class, set the metadata by calling its
`fdf_metadata::MetadataServer::ForwardMetadata()` method, and then serve the
metadata to the driver's outgoing directory by calling its
`fdf_metadata::MetadataServer::Serve()` method:

```cpp
// Make sure to include the metadata library that specializes the
// `fdf_metadata::ObjectDetails` class. It is needed by
// `fdf_metadata::MetadataServer`.
#include "examples_metadata.h"

#include <lib/driver/metadata/cpp/metadata_server.h>

class Forwarder : public fdf::DriverBase {
 public:
  zx::result<> Start() override {
    // Set metadata using the driver's parent driver metadata.
    ZX_ASSERT(metadata_server_.ForwardMetadata(incoming()) == ZX_OK);

    // Serve the metadata to the driver's outgoing directory.
    ZX_ASSERT(metadata_server_.Serve(*outgoing(), dispatcher()) == ZX_OK);

    return zx::ok();
  }

 private:
  // Responsible for serving metadata.
  fdf_metadata::MetadataServer<fuchsia_examples_metadata::Metadata> metadata_server_;
};
```

It should be noted that `fdf_metadata::MetadataServer::ForwardMetadata()` does
not check if the parent driver has changed what metadata it provides after
`fdf_metadata::MetadataServer::ForwardMetadata()` has been called. The driver
will have to call `fdf_metadata::MetadataServer::ForwardMetadata()` again in
order to incorporate the change.

The `driver` build target will need to be updated:

```
fuchsia_cc_driver("driver") {
  testonly = true
  output_name = "forward_driver"
  sources = [
    "forward_driver.cc",
  ]
  deps = [
    "//src/devices/lib/driver:driver_runtime",
    ":examples_metadata",
  ]
}
```

### Exposing and using the metadata service
Finally, the driver will need to declare, use, and expose the
`fuchsia.examples.metadata.Metadata` FIDL service in its component manifest:

```
{
    include: [
        "inspect/client.shard.cml",
        "syslog/client.shard.cml",
    ],
    program: {
        runner: "driver",
        binary: "driver/forward_driver.so",
        bind: "meta/bind/forward.bindbc",
    },
    capabilities: [
        { service: "fuchsia.examples.metadata.Metadata" },
    ],
    expose: [
        {
            service: "fuchsia.examples.metadata.Metadata",
            from: "self",
        },
    ],
    use: [
        { service: "fuchsia.examples.metadata.Metadata" },
    ],
}
```
