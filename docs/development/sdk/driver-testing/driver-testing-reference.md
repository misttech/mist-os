# Driver testing reference

## TestNode

This class serves as the server for two FIDLs that are provided by the driver
framework's driver manager:

-   `fuchsia.driver.framework/Node`
-   `fuchsia.driver.framework/NodeController`

The Node FIDL is how drivers communicate with the driver framework to add child
nodes into the driver topology. The `NodeController protocol`
is how a driver can manage the children nodes that it has created.

The [TestNode class](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/testing/cpp/test_node.cc)
is a part of the unit test's environment that is given
to the driver being tested. This is done through the
`fuchsia.driver.framework/DriverStartArgs` FIDL table.
To simplify the creation of this type and have easy access to the other ends
of channels the driver is provided with through this,
there is a struct defined in this class called
`CreateStartArgsResult` and a corresponding method.

```cpp
struct CreateStartArgsResult {
  fuchsia_driver_framework::DriverStartArgs start_args;
  fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server;
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
};

zx::result<CreateStartArgsResult> CreateStartArgsAndServe();
```

This method is the starting point for a test to create the `DriverStartArgs`
that will later be provided to the driver.
The values in the struct are as follows:

-   **`start_args`**: The actual start args table to be given to the driver.
-   **`incoming_directory_server`**: Drivers require an incoming directory to
    provide them with the various FIDLs that they need (these are defined in
    the driver's cml file with using statements). This is the server end for
    that incoming directory, which can be provided to the driver using the
    `TestEnvironment` class. This server end must be given to
    `TestEnvironment::initialize`.
-   **`outgoing_directory_client`**: Drivers themselves provide various FIDLs
    back out to the system and other drivers through an outgoing directory
    (these are defined in the driver's cml as capabilities/exposed). This
    client end is the other side of that outgoing directory and can be used by
    the test to use these driver provided FIDLs.

## TestEnvironment

The [TestEnvironment class](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/testing/cpp/test_environment.cc)
is used to provide the driver's incoming directory. This includes any FIDL
that the driver needs to function, at least any of them that it needs
to function just for the unit test, not necessarily everything.
Adding into the `TestEnvironment` requires grabbing the
`fdf::OutgoingDirectory` that is wrapped inside of it
through the `incoming_directory()` call.

Note: the confusing outgoing/incoming terms being used together here is because
`OutgoingDirectory` is the name of the class that can back FIDL servers,
which are generally done by components as part of providing an
outgoing namespace. But this is outgoing from the test, into the driver, and
so it is the driver's incoming namespace that it is providing.

The interface to `fdf::OutgoingDirectory` is very similar to that of
`component::OutgoingDirectory`, except that it also allows providing driver
transport based services along with plain zircon channel based services. Drivers
are discouraged from using protocols, but instead should use services; therefore,
this only provides `AddService`/`RemoveService` calls. If a plain protocol must
be added, the internal `component::OutgoingDirectory` can be accessed using the
`component() `function.

## DriverUnderTest

This class is a RAII wrapper for the driver being tested. It provides the
various lifecycle hooks for the driver, and arrow and star operators for
accessing the underlying driver pointer (the type is provided as a template
argument, void type is used when no type is provided).

On destruction, it will call |Stop| for the driver if it hasn't already been
called, but |PrepareStop| must have been manually called and awaited by the
test.

This class works by using the C lifecycle definition that is exported. Most
drivers have already done this through the `FUCHSIA_DRIVER_EXPORT` macro, although
a custom one can be provided if desired.

The lifecycle interface for this is as follows:

```cpp
// Start the driver. This is an asynchronous operation.
// Use |DriverRuntime::RunToCompletion| to await the completion of the async task.
// The resulting zx::result is the result of the start operation.
DriverRuntime::AsyncTask<zx::result<>> Start(fdf::DriverStartArgs start_args);

// PrepareStop the driver. This is an asynchronous operation.
// Use |DriverRuntime::RunToCompletion| to await the completion of the async task.
// The resulting zx::result is the result of the prepare stop operation.
DriverRuntime::AsyncTask<zx::result<>> PrepareStop();

// Stop the driver. The PrepareStop operation must have been completed before Stop
// is called.
// Returns the result of the stop operation.
zx::result<> Stop();
```

## DriverRuntime

The [DriverRuntime class](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/sdk/lib/driver/testing/cpp/driver_runtime.cc)
is used by the test as a client into the features that are provided
by the driver runtime. The driver runtime includes things like the
background managed thread pool for background driver dispatchers,
support for driver transport protocols, creating dispatchers, and
running the foreground attached dispatcher.

Test authors should be mindful of the various pieces they have in their tests,
how they interact, and what dispatcher they are using to back each piece.
Otherwise they can introduce memory safety issues, deadlocks, race conditions,
and flaky tests.

### Foreground dispatcher

One area that can cause confusion for driver unit test authors is the threading
model. On a full system, the dispatchers provided by the driver runtime are all
run on a shared thread pool that is managed by the driver runtime.
This model makes interacting safely with the driver
through the test more difficult, as the test code is running
on the foreground (initial/main) thread. To solve this problem,
the driver runtime has been modified to provide a test-specific dispatcher
that is meant to be run on the foreground thread, and
not scheduled to run on its background managed thread pool.

The foreground dispatcher is a special type of dispatcher that was added to the
driver runtime specifically for unit tests. It is attached to the test's main
thread as the current dispatcher (`fdf::Dispatcher::GetCurrent() `can get it).
Tasks that are posted to this dispatcher must be manually run using the
various Run methods in the `DriverRuntime`. It allows for thread safe direct
access for objects that are set to live on it.

Generally the driver being tested should be put on the foreground dispatcher so
that methods on the driver can be called directly by the test. The only
situation where this shouldn't be the case is if the test wants to make sync
FIDL calls into the driver's outgoing directory from the test thread.

There can only be 1 foreground dispatcher created. This dispatcher is
automatically created when the `DriverRuntime` class is constructed.
The user does not have to manually create the foreground dispatcher.
The foreground dispatcher is torn down during the destruction
of the `DriverRuntime` class.

### Background dispatchers

Background dispatchers are plain driver dispatchers that are used by drivers on
the full system. These dispatchers share a common thread pool that the driver
runtime manages the number of threads for. The same thread in the thread pool
can be used to run any dispatcher, and each dispatcher can end up running on any
thread in the pool.

With a synchronized dispatcher, there is a guarantee that no two threads will
be executing the dispatcher at the same time.

The threading model of driver dispatchers are discussed in-depth in the
[driver runtime RFC](/docs/contribute/governance/rfcs/0126_driver_runtime.md) and the
[dispatcher documentation](/docs/concepts/drivers/driver-dispatcher-and-threads.md).

Tests can create any number of background dispatchers using this `DriverRuntime`
function:

```cpp
fdf::UnownedSynchronizedDispatcher StartBackgroundDispatcher();
```

The dispatcher created here will be owned by the `DriverRuntime` instance and
will be torn down during the destructor of the `DriverRuntime` class.

Generally the pieces of the test environment (`TestNode` and `TestEnvironment`
instances) should live on a background dispatcher. This is to make sync calls
from the driver into the environment be allowed. Otherwise the driver must
ensure to only make async calls into the environment.

Class instances that live on a background dispatcher (and are marked as
thread-unsafe) are not safe for direct access through the test thread. These
should be wrapped inside of an `async_patterns::TestDispatcherBound` type, which
ensures thread safety.

Note: If a driver is creating its own dispatcher as part of normal operation,
then that dispatcher will be a background dispatcher, even if the driver itself
starts out on the foreground dispatcher.

### Parallels to async::Loop

The design of the `DriverRuntime` takes some inspiration from the `async::Loop`
that other Fuchsia components use. The `DriverRuntime` object itself is a
similar idea to the `async::Loop` object.
It is used to get dispatchers and it is the executor of the dispatchers
that are created by it using various Run methods.

The foreground dispatcher is the same as creating an `async::Loop` with the
`kAsyncLoopConfigAttachToCurrentThread` configuration. The loop dispatcher must
be manually run using the various Run methods available on the `async::Loop`
object. The `DriverRuntime` has all the same Run methods as `async::Loop`. The
dispatcher can be accessed with `fdf::Dispatcher::GetCurrent()` similar to how
the attached loop dispatcher can be grabbed with the default dispatcher getter
`async_get_default_dispatcher()`.

Starting background dispatchers is similar to creating an `async::Loop` with
the `kAsyncLoopConfigNoAttachToCurrentThread` configuration and calling
`StartThread` on the loop to create a background thread that runs the loop
dispatcher. The background dispatcher can be accessed while it is running with
the `fdf::Dispatcher::GetCurrent()`, similar to how the `async::Loop` dispatcher
could be accessed using the default dispatcher getter
`async_get_default_dispatcher()`. While `async::Loop` would allow running these
manually as well (using the various Run methods) for a multi-threaded
dispatcher, the `DriverRuntime` does not allow this.

The main difference here is that the `DriverRuntime` can create both types of
these dispatchers and create and own multiple background dispatchers. With
`async::Loop`, there is a 1:1 relationship with the loop and dispatcher so for
each dispatcher, a new `async::Loop` has to be created with the configuration
for what type of dispatcher is needed.
