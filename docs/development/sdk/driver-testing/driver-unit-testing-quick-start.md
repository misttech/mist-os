# Driver unit testing quick start

Once you are familiar with the Driver Framework v2 testing framework, follow
this quick start to write a test for drivers that need to make synchronous FIDL
(see
[driver FIDL test code](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc)).
If your driver doesn't need to make synchronous FIDL calls, see the
[driver base test](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_base_test.cc).

## Include library dependencies

To test drivers, unit tests need to have access to the various resources and
environments needed by the drivers themselves. For example, these are the
library dependencies for the
[Driver FIDL test](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc):

```cpp
#include <fidl/fuchsia.driver.component.test/cpp/driver/wire.h>
#include <fidl/fuchsia.driver.component.test/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/component/cpp/tests/test_driver.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdio/directory.h>
#include <gtest/gtest.h>
```

[DispatcherBound](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/async_patterns/cpp/dispatcher_bound.h)
is required to test how the driver uses FIDL clients when calling into its
environment. It ensures a server-end object runs in a single dispatcher,
enabling the test to construct, call methods on, and destroy the object used
(see [Threading tips in tests](/docs/development/drivers/testing/threading-tips-in-tests.md)).

[GTest Runner](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/src/sys/test_runners/gtest/README.md)
is a test runner that launches a `gtest` binary as a component, parses its
output, and translates it to `fuchsia.test.Suite` protocol on behalf of the
test.

## Provide handler to easily add server bindings

It's good practice for servers to provide a `GetInstanceHandler` to easily add
server bindings and run them off a binding group. The bindings should be added
on the current driver dispatcher. The expectation is that this class is run
inside of a dispatcher bound to the environment. For example,
[Driver FIDL test, line 35-52](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#35),
provides a handler to the driver service:

```
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc" region_tag="provide_handler" adjust_indentation="auto" %}
```

## Set up testing framework

### Create driver runtime

Creating the driver runtime automatically attaches a foreground dispatcher,
[Driver FIDL test, line 115](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#115):

```cpp
fdf_testing::DriverRuntime runtime_;
```

### Start background dispatcher

The driver dispatcher is set as a background dispatcher,
[Driver FIDL test, line 118](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#118):

```cpp {:.devsite-disable-click-to-copy}
fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
```

### Create TestNode object

The test node serves the `fdf::Node protocol` to the driver,
[Driver FIDL test, lines 127-128](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#127):

```cpp
async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
    env_dispatcher(), std::in_place, std::string("root")};
```

### Create TestEnvironment object

The environment can serve both the Zircon and Driver transport based protocols
to the driver,
[Driver FIDL test, lines 131-132](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#131):

```cpp
async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
    env_dispatcher(), std::in_place};
```

### Create custom FIDL server

The custom FIDL server lives on the background environment dispatcher and has
to be wrapped in a dispatcher bound,
[Driver FIDL test, lines 121-124](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#121):

```
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc" region_tag="custom_server_classes" adjust_indentation="auto" %}
```

### Get custom FIDL server handler

Get the instance handler for the driver protocol,
[Driver FIDL test, lines 71-74](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#71):

```
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc" region_tag="get_server_handlers" adjust_indentation="auto" %}
```

### Move custom FIDL server handler

Move the instance handler into our driver's incoming namespace,
[Driver FIDL test, lines 76-87](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#76):

```
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc" region_tag="move_server_handlers" adjust_indentation="auto" %}
```

### Call CreateStartArgsAndServe

Create and serve the `start_args table`,
[Driver FIDL test, lines 59-60](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#59):

```c++
zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
ASSERT_EQ(ZX_OK, start_args.status_value());
```

### Initialize test environment

Initialize the test environment,
[Driver FIDL test, lines 65-68](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#65):

```
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc" region_tag="initialize_test_environment" adjust_indentation="auto" %}
```

## Run tests

### Add the driver under test

Add the driver under test which will use the foreground dispatcher,
[Driver FIDL test, lines 167](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#167):

```cpp
fdf_testing::DriverUnderTest<TestDriver> driver_;
```

### Start driver

Start the driver,
[Driver FIDL test, lines 237-238](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#237):

```cpp
zx::result result = runtime().RunToCompletion(driver_.SyncCall(
  &fdf_testing::DriverUnderTest<TestDriver>::Start, std::move(start_args())));
```

### Add tests

Use the arrow operator on the `DriverUnderTest` to add tests for the driver.
The arrow operator gives access to the driver type
(specified in the `DriverUnderTest` template), for example,
[Driver FIDL test, lines 384-287](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#284):

```cpp
driver().SyncCall([](fdf_testing::DriverUnderTest<TestDriver>* driver) {
  zx::result result = (*driver)->ServeDriverService();
  ASSERT_EQ(ZX_OK, result.status_value());
});
```

### Call PrepareStop

`PrepareStop` has to be called manually by tests,
[Driver FIDL test, line 159](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/component/cpp/tests/driver_fidl_test.cc#159):

```cpp
zx::result result = runtime().RunToCompletion(driver_.PrepareStop());
```

### Run unit tests

Execute the following command to run the driver tests
(for the iwlwifi driver):

```posix-terminal
tools/bazel test third_party/iwlwifi/test:iwlwifi_test_pkg
```
