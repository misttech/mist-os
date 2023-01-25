// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_DRIVER_BASE_H_
#define LIB_DRIVER_COMPONENT_CPP_DRIVER_BASE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/component/outgoing/cpp/structured_config.h>
#include <lib/driver/component/cpp/driver_context.h>
#include <lib/driver/component/cpp/prepare_stop_completer.h>
#include <lib/driver/component/cpp/start_args.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>

namespace fdf {

using DriverStartArgs = fuchsia_driver_framework::DriverStartArgs;

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
//     context().incoming()->Connect(...);
//     context().outgoing()->AddService(...);
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
             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : name_(name),
        start_args_(std::move(start_args)),
        driver_dispatcher_(std::move(driver_dispatcher)),
        dispatcher_(driver_dispatcher_->async_dispatcher()),
        driver_context_(driver_dispatcher_->get()) {
    auto ns = std::move(start_args_.incoming());
    ZX_ASSERT(ns.has_value());
    Namespace incoming = Namespace::Create(ns.value()).value();
    logger_ = Logger::Create(incoming, dispatcher_, name_).value();

    auto outgoing_request = std::move(start_args_.outgoing_dir());
    ZX_ASSERT(outgoing_request.has_value());
    driver_context_.InitializeAndServe(std::move(incoming), std::move(outgoing_request.value()));
  }

  DriverBase(const DriverBase&) = delete;
  DriverBase& operator=(const DriverBase&) = delete;

  // The destructor is called right after the |Stop| method.
  virtual ~DriverBase() = default;

  // This method will be called by the factory to start the driver. This is when
  // the driver should setup the outgoing directory through `context().outgoing()->Add...` calls.
  // Do not call Serve, as it has already been called by the |DriverBase| constructor.
  // Child nodes can be created here synchronously or asynchronously as long as all of the
  // protocols being offered to the child has been added to the outgoing directory first.
  virtual zx::result<> Start() = 0;

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

  std::string_view name() const { return name_; }

  DriverContext& context() { return driver_context_; }
  const DriverContext& context() const { return driver_context_; }

  fdf::UnownedSynchronizedDispatcher& driver_dispatcher() { return driver_dispatcher_; }
  const fdf::UnownedSynchronizedDispatcher& driver_dispatcher() const { return driver_dispatcher_; }

  async_dispatcher_t* dispatcher() { return dispatcher_; }
  const async_dispatcher_t* dispatcher() const { return dispatcher_; }

  std::optional<fuchsia_data::Dictionary>& program() { return start_args_.program(); }
  const std::optional<fuchsia_data::Dictionary>& program() const { return start_args_.program(); }

  std::optional<std::string>& url() { return start_args_.url(); }
  const std::optional<std::string>& url() const { return start_args_.url(); }

  std::optional<std::string>& node_name() { return start_args_.node_name(); }
  const std::optional<std::string>& node_name() const { return start_args_.node_name(); }

  std::optional<std::vector<fuchsia_driver_framework::NodeSymbol>>& symbols() {
    return start_args_.symbols();
  }

  const std::optional<std::vector<fuchsia_driver_framework::NodeSymbol>>& symbols() const {
    return start_args_.symbols();
  }

 private:
  std::string name_;
  DriverStartArgs start_args_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_dispatcher_t* dispatcher_;
  DriverContext driver_context_;
};

}  // namespace fdf

#endif  // LIB_DRIVER_COMPONENT_CPP_DRIVER_BASE_H_
