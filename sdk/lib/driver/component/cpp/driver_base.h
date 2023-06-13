// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_DRIVER_BASE_H_
#define LIB_DRIVER_COMPONENT_CPP_DRIVER_BASE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/component/outgoing/cpp/structured_config.h>
#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>

namespace fdf {

using DriverStartArgs = fuchsia_driver_framework::DriverStartArgs;

// Used to indicate if we should wait for the initial interest change for the driver's logger.
extern bool logger_wait_for_initial_interest;

// |DriverBase| is an interface that drivers should inherit from. It provides methods
// for accessing the start args, as well as helper methods for common initialization tasks.
//
// There are four virtual methods:
// |Start| which must be overridden.
// |PrepareStop|, |Stop|, and the destructor |~DriverBase|, are optional to override.
//
// In order to work with the default |BasicFactory| factory implementation,
// classes which inherit from |DriverBase| must implement a constructor with the following
// signature and forward said parameters to the |DriverBase| base class:
//
//   T(DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);
//
// Otherwise a custom factory must be created and used to call constructors of any other shape.
//
// The following illustrates an example:
//
// ```
// class MyDriver : public fdf::DriverBase {
//  public:
//   MyDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
//       : fdf::DriverBase("my_driver", std::move(start_args), std::move(driver_dispatcher)) {}
//
//   zx::result<> Start() override {
//     incoming()->Connect(...);
//     outgoing()->AddService(...);
//     FDF_LOG(INFO, "hello world!");
//     node_client_.Bind(std::move(node()), dispatcher());
//
//     /* Ensure all capabilities offered have been added to the outgoing directory first. */
//     auto add_result = node_client_->AddChild(...); if (add_result.is_error()) {
//       /* Releasing the node channel signals unbind to DF. */
//       node_client_.AsyncTeardown(); // Or node().reset() if we hadn't moved it into the client.
//       return add_result.take_error();
//     }
//
//     return zx::ok();
//   }
//  private:
//   fidl::SharedClient<fuchsia_driver_framework::Node> node_client_;
// };
// ```
//
// # Thread safety
//
// This class is thread-unsafe. Instances must be managed and used from tasks
// running on the |driver_dispatcher|, and the dispatcher must be synchronized.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
class DriverBase {
 public:
  DriverBase(std::string_view name, DriverStartArgs start_args,
             fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  DriverBase(const DriverBase&) = delete;
  DriverBase& operator=(const DriverBase&) = delete;

  // The destructor is called right after the |Stop| method.
  virtual ~DriverBase() = default;

  // This method will be called by the factory to start the driver. This is when
  // the driver should setup the outgoing directory through `outgoing()->Add...` calls.
  // Do not call Serve, as it has already been called by the |DriverBase| constructor.
  // Child nodes can be created here synchronously or asynchronously as long as all of the
  // protocols being offered to the child has been added to the outgoing directory first.
  // There are two versions of this method which may be implemented depending on whether Start would
  // like to complete synchronously or asynchronously. The driver may override either one of these
  // methods, but must implement one. The asynchronous version will be called over the synchronous
  // version if both are implemented.
  virtual zx::result<> Start() { return zx::error(ZX_ERR_NOT_SUPPORTED); }
  virtual void Start(StartCompleter completer) { completer(Start()); }

  // This provides a way for the driver to asynchronously prepare to stop. The driver should
  // initiate any teardowns that need to happen on the driver dispatchers. Once it is ready to stop,
  // the completer's Complete function can be called (from any thread/context) with a result.
  // After the completer is called, the framework will shutdown all of the driver's fdf dispatchers
  // and deallocate the driver.
  virtual void PrepareStop(PrepareStopCompleter completer) { completer(zx::ok()); }

  // This is called after all the driver dispatchers belonging to this driver have been shutdown.
  // This ensures that there are no pending tasks on any of the driver dispatchers that will access
  // the driver after it has been destroyed.
  virtual void Stop() {}

  // This can be used to log in driver factories:
  // `FDF_LOGL(INFO, driver->logger(), "...");`
  Logger& logger() { return *logger_; }

 protected:
  // The logger can't be private because the logging macros rely on it.
  // NOLINTNEXTLINE(misc-non-private-member-variables-in-classes)
  std::unique_ptr<Logger> logger_;

  // Client to the `fuchsia.driver.framework/Node` protocol provided by the driver framework.
  // This can be used to add children to the node that the driver is bound to.
  fidl::ClientEnd<fuchsia_driver_framework::Node>& node() {
    auto& node = start_args_.node();
    ZX_ASSERT(node.has_value());
    return node.value();
  }

  const fidl::ClientEnd<fuchsia_driver_framework::Node>& node() const {
    auto& node = start_args_.node();
    ZX_ASSERT(node.has_value());
    return node.value();
  }

  template <typename StructuredConfig>
  StructuredConfig take_config() {
    static_assert(component::IsDriverStructuredConfigV<StructuredConfig>,
                  "Invalid type supplied. StructuredConfig must be a driver flavored "
                  "structured config type. Example usage: take_config<my_driverconfig::Config>().");
    return StructuredConfig::TakeFromStartArgs(start_args_);
  }

  // The name of the driver that is given to the DriverBase constructor.
  std::string_view name() const { return name_; }

  // Used to access the incoming namespace of the driver. This allows connecting to both zircon and
  // driver transport incoming services.
  const std::shared_ptr<Namespace>& incoming() const { return incoming_; }

  // The `/svc` directory in the incoming namespace.
  fidl::UnownedClientEnd<fuchsia_io::Directory> svc() const { return incoming_->svc_dir(); }

  // Used to access the outgoing directory that the driver is serving. Can be used to add both
  // zircon and driver transport outgoing services.
  std::shared_ptr<OutgoingDirectory>& outgoing() { return outgoing_; }

  // The unowned synchronized driver dispatcher that the driver is started with.
  const fdf::UnownedSynchronizedDispatcher& driver_dispatcher() const { return driver_dispatcher_; }

  // The async_dispatcher_t interface of the synchronized driver dispatcher that the driver
  // is started with.
  async_dispatcher_t* dispatcher() const { return dispatcher_; }

  // The program dictionary in the start args.
  // This is the `program` entry in the cml of the driver.
  const std::optional<fuchsia_data::Dictionary>& program() const { return start_args_.program(); }

  // The url field in the start args.
  // This is the URL of the package containing the driver. This is purely informational,
  // used only to provide data for inspect.
  const std::optional<std::string>& url() const { return start_args_.url(); }

  // The node_name field in the start args.
  // This is the name of the node that the driver is bound to.
  const std::optional<std::string>& node_name() const { return start_args_.node_name(); }

  // The symbols field in the start args.
  // These come from the driver that added |node|, and are filtered to the symbols requested in the
  // bind program.
  const std::optional<std::vector<fuchsia_driver_framework::NodeSymbol>>& symbols() const {
    return start_args_.symbols();
  }

 private:
  void InitializeAndServe(Namespace incoming,
                          fidl::ServerEnd<fuchsia_io::Directory> outgoing_directory_request);

  std::string name_;
  DriverStartArgs start_args_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_dispatcher_t* dispatcher_;
  std::shared_ptr<Namespace> incoming_;
  std::shared_ptr<OutgoingDirectory> outgoing_;
};

}  // namespace fdf

#endif  // LIB_DRIVER_COMPONENT_CPP_DRIVER_BASE_H_
