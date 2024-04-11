# Driver unit testing quick start

Follow this quick start to write a driver unit test based on the
[simple unit test code example](https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/simple/dfv2/tests/test.cc):

## Include library dependencies

Include this library dependency:

```cpp
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>
```

The [DriverTestFixture](#drivertestfixture-configuration-arguments)
is a base class that driver unit test fixture classes can inherit from.
Tests define a configuration class to pass into the fixture
through a template parameter.
The fixture takes care of setting up the test environment and driver
on the correct dispatchers, starting and stoping the driver as requested.

## Create fixture configuration class

The `DriverTestFixture` base class takes in a configuration class
through a template parameter.
This configuration class must be provided with certain values
that dictate how the test should run.

Here is an example of a configuration class:

```cpp
class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = simple::SimpleDriver;
  using EnvironmentType = SimpleDriverTestEnvironment;
};
```

## Define environment type class

The `EnvironmentType` must be an isolated class
that provides your driver’s custom dependencies.
It does not need to provide framework dependencies (except for `compat::DeviceServer`),
as the fixture does that already.
If no extra dependencies are needed, use `fdf_testing::MinimalEnvironment`
which provides a default `compat::DeviceServer`.

Here's an example of a basic test with the minimal environment:

```cpp
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = MyDriverType;
  using EnvironmentType = fdf_testing::MinimalEnvironment;
};

class MyFixture : public fdf_testing::DriverTestFixture<FixtureConfiguration> {};

TEST_F(MyFixture, MyTest) {
  driver().DoSomething();
}
```

## Run unit tests

Driver unit tests are executed from within the test folder of the driver itself.
For example, execute the following command to run the driver tests
for the iwlwifi driver:

```posix-terminal
tools/bazel test third_party/iwlwifi/test:iwlwifi_test_pkg
```

## DriverTestFixture configuration arguments

### DriverType

The type of the driver under test. This must be an inheritor of `fdf::DriverBase`.

Use `DriverType` to define the reference type for only the fixture's functions
(for example, `driver()` and `RunInDriverContext()`).
Use the driver registration symbol (created by the `FUCHSIA_DRIVER_EXPORT` macro)
for the driver lifecycle management.
When using a custom driver type,
ensure the custom `DriverType` contains a public static function
with the signature shown below:

```cpp
static DriverRegistration GetDriverRegistration()
```

The test uses this registration instead to manage the driver lifecycle.

### EnvironmentType
A class that contains custom dependencies for the driver under test.
The environment will always live on a background dispatcher.

It must be default constructible, derive from the `fdf_testing::Environment class`,
and override the following function:
`zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override;`

The function is called automatically on the background environment dispatcher
during initialization.
It must add its components into the provided `fdf::OutgoingDirectory object`,
generally done through the `AddService` method.
The `OutgoingDirectory` backs the driver's incoming namespace, hence its name,
`to_driver_vfs`.

Example custom environment:

```cpp
class MyFidlServer : public fidl::WireServer<fuchsia_examples_gizmo::Proto> {...};

class CustomEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) {
    device_server_.Init(component::kDefaultInstance, "root");
    EXPECT_EQ(ZX_OK, device_server_.Serve(
    fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs));

    EXPECT_EQ(ZX_OK, to_driver_vfs.AddService<fuchsia_examples_gizmo::Service::Proto>(
      custom_server_.CreateInstanceHandler()).status_value());

    return zx::ok();
  }

 private:
  compat::DeviceServer device_server_;
 MyFidlServer custom_server_;
};
```

### kDriverOnForeground (prefer = true)

Whether to have the driver under test run on the foreground dispatcher,
or to run it on a dedicated background dispatcher.

When this is true, the test can access the driver under test
using the `driver()` method and directly make calls into it,
but sync client tasks must go through `RunInBackground()`.

When this is false, the test can run tasks on the driver context
using the `RunInDriverContext()` methods,
but sync client tasks can be run directly.

### kAutoStartDriver (prefer = true)

If true, the test will automatically start the driver
on construction of `DriverTestFixture`, and expect a successful start.

### kAutoStopDriver (prefer = true)

If true, the test will automatically stop the driver
on destruction of `DriverTestFixture`, and expect a successful stop.

## Methods available to the test under all configs

The following methods are available to the test under all configurations.

### runtime

Access the driver runtime object.
This can be used to create new background dispatchers or
to run the foreground dispatcher.
The user does not need to explicitly create dispatchers for the environment or
the driver as the fixture has already done that.

### Connect

Connects to an instance of a service member that the driver under test provides.
This can be either a driver transport or a zircon channel transport based service.

### ConnectThroughDevfs

Connects to a protocol that the driver has exported through `devfs`.
This can be given the node_name of the `devfs` node,
or a list of node names to traverse before reaching the `devfs` node.

### RunInEnvironmentTypeContext

Runs a task on the `EnvironmentType` instance that the test is using.

### RunInNodeContext

Runs a task on the `fdf_testing::TestNode` instance that the test is using.
This can be used to validate the driver’s interactions with the driver framework node
(like checking how many children have been added).

## Methods Available to the Test With config-based restrictions

The following methods are available to the test
with certain config-based restrictions (in parentheses).

### StartDriver and StartDriverCustomized (kAutoStartDriver = false)

This can be used to manually start the driver under test.
Should only be used if `kAutoStartDriver` is false.
The customized variant can be used to modify the start arguments
that the driver is given.

### StopDriver (kAutoStopDriver = false)

This can be used to manually stop the driver under test.
Should only be used if `kAutoStopDriver` is false.

### RunInDriverContext (kDriverOnForeground = false)

This can be used to run a callback on the driver under test.
The callback input will have a reference to the driver.
All accesses to the driver must go through this as it is unsafe to touch the driver
on the main test thread under this configuration.

### driver (kDriverOnForeground = true)

This can be used to access the driver directly from the test.
Since the driver is on the foreground it is safe to access this
on the main test thread.

### RunInBackground (kDriverOnForeground = true)

Runs a task on a background dispatcher, separate from the driver.
This is done to avoid deadlocking with the driver when making sync client calls
with the `kDriverOnForeground` configuration.

### Run* functions warning

Be careful when using the Run* functions
(`RunInDriverContext`, `RunInBackground`, `RunInEnvironmentTypeContext`, `RunInNodeContext`).
These tasks run on specific dispatchers, so it might be unsafe to:

* Pass raw pointers into them from another context
(main thread or a different Run* kind) to use in the function
* Return a raw pointer (through a captured ref or return type) out of them
to use on the main thread or to capture/use in another Run* function
(except for a Run* function of the same kind).

## Examples

* [Simple driver unit test](https://cs.opensource.google/fuchsia/fuchsia/+/main:examples/drivers/simple/dfv2/tests/test.cc)
* [fxr/997660](http://fxr/997660)
* [fxr/1001356](http://fxr/1001356)
* [fxr/996165](http://fxr/996165)