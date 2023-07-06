// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_TESTING_LIB_DFV2_SIM_DRIVER_HOST_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_TESTING_LIB_DFV2_SIM_DRIVER_HOST_H_

#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/sync/cpp/completion.h>

#include <utility>

namespace wlan::simulation {

#define DISPATCHER_SYNC_TIMEOUT zx::sec(5)
#define NODE_REMOVAL_TIMEOUT zx::sec(2)

template <typename Protocol>
component::TypedHandler<Protocol> RouteToIncoming() {
  return [](fidl::ServerEnd<Protocol> server_end) {
    auto result = component::Connect<Protocol>(std::move(server_end));
    ASSERT_EQ(ZX_OK, result.status_value());
  };
}

auto PrepareStopCallback(void* cookie, zx_status_t status) { *(static_cast<bool*>(cookie)) = true; }

using HostRemoveNodeCallback = std::function<void()>;

class SimNode : public fidl::WireServer<fuchsia_driver_framework::NodeController> {
 public:
  explicit SimNode(fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller_server_end,
                   HostRemoveNodeCallback callback) {
    host_callback_ = callback;

    node_controller_loop_ = new async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    zx_status_t status = node_controller_loop_->StartThread("node-server-loop");
    ZX_ASSERT(status == ZX_OK);
    node_controller_dispatcher_ = node_controller_loop_->dispatcher();

    fidl::BindServer(node_controller_dispatcher_, std::move(controller_server_end), this);
  }

  ~SimNode() {
    node_controller_loop_->Shutdown();
    delete node_controller_loop_;
  }

  void Remove(RemoveCompleter::Sync& completer) override {
    ZX_ASSERT(host_callback_);
    host_callback_();
  }

  void RequestBind(RequestBindRequestView request, RequestBindCompleter::Sync& completer) override {
    // Do nothing.
    completer.ReplySuccess();
  }

 private:
  HostRemoveNodeCallback host_callback_;
  async::Loop* node_controller_loop_ = nullptr;
  async_dispatcher_t* node_controller_dispatcher_ = nullptr;
};

// The Fake driver host for a certain type of driver.
template <typename Driver, typename ParentService, typename ParentProtocol>
class SimDriverHost : public fidl::WireServer<fuchsia_driver_framework::Node> {
 public:
  SimDriverHost() {
    // Create the dispatcher to start a driver. Passing FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS as
    // the option to assign a new thread for this dispatcher, so that the sync call on this
    // dispatcher won't cause deadlock due to reentrancy.
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        {.value = FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS}, "sim-driver-dispatcher",
        [this](fdf_dispatcher_t* dispatcher) { shutdown_completion_.Signal(); });
    ZX_ASSERT_MSG(!dispatcher.is_error(), "Creating dispatcher error: %s",
                  zx_status_get_string(dispatcher.status_value()));

    driver_dispatcher_ = std::move(*dispatcher);

    node_loop_ = new async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
    zx_status_t status = node_loop_->StartThread("node-server-loop");
    ZX_ASSERT(status == ZX_OK);
    host_dispatcher_ = node_loop_->dispatcher();
  }

  ~SimDriverHost() {
    // Clear the setups in the host dispatcher.
    zx_status_t wait_status;
    libsync::Completion destruct_completion;
    auto destruct_task = [this, &destruct_completion]() {
      parent_server_.reset();
      host_outgoing_dir_.reset();
      destruct_completion.Signal();
    };
    async::PostTask(host_dispatcher_, std::move(destruct_task));
    wait_status = destruct_completion.Wait(DISPATCHER_SYNC_TIMEOUT);
    if (wait_status == ZX_ERR_TIMED_OUT) {
      ZX_PANIC("Timeout waiting on host dispatcher.");
    }

    // Delete the hosted driver in driver dispatcher.
    destruct_completion.Reset();
    auto delete_driver_task = [this, &destruct_completion] {
      hosted_driver_.reset();
      destruct_completion.Signal();
    };
    async::PostTask(driver_dispatcher_.async_dispatcher(), std::move(delete_driver_task));
    wait_status = destruct_completion.Wait(DISPATCHER_SYNC_TIMEOUT);
    if (wait_status == ZX_ERR_TIMED_OUT) {
      ZX_PANIC("Timeout waiting on driver dispatcher.");
    }

    node_loop_->Shutdown();
    delete node_loop_;
    driver_dispatcher_.ShutdownAsync();
    wait_status = shutdown_completion_.Wait(DISPATCHER_SYNC_TIMEOUT);
    if (wait_status == ZX_ERR_TIMED_OUT) {
      ZX_PANIC("Timeout waiting on driver dispatcher shutdown.");
    }
  }

  // Implementation of fuchsia_driver_framework::Node.
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override {
    node_list_.push_back(std::move(
        std::make_unique<SimNode>(std::move(request->controller), [this]() { RemoveChild(); })));
    child_node_count_++;
    completer.ReplySuccess();
  }

  void RemoveChild() {
    // node_removal_completion_.Reset();
    child_node_count_--;
    // Fire a signal, don't have to be waited.
    node_removal_completion_.Signal();

    // This operation will destroy the thread that this function itself is running on, thus do it
    // lastly.
    node_list_.pop_back();
  }

  struct StartCompleterContext {
    fdf::DriverBase* driver_ptr;
    zx_status_t status;
  };

  // Prepare the context and create the driver inside this host.
  void StartDriver(std::unique_ptr<fidl::WireServer<ParentProtocol>> parent_server) {
    parent_server_ = std::move(parent_server);
    fuchsia_driver_framework::DriverStartArgs start_args;

    // Setup DriverStartArgs on setup dispatcher.
    libsync::Completion setup_completion;
    auto setup_task = [this, &start_args, &setup_completion]() {
      /* Prepare "node" field in DriverStartArgs */

      // Create the endpoints of Node protocol.
      auto node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
      ZX_ASSERT_MSG(!node_endpoints.is_error(), "Creating node endpoints error: %s",
                    zx_status_get_string(node_endpoints.status_value()));
      // Bind the ServerEnd and implement the FIDL call handlers in this class.
      fidl::BindServer(host_dispatcher_, std::move(node_endpoints->server), this);

      /* Prepare "outgoing_dir" field in DriverStartArgs */

      // Fake the driver outgoing directory endpoints for the driver being created. ServerEnd will
      // be passed to DriverStartArgs and served to the driver's own outgoing directory(not the one
      // that we create in this class), ClientEnd will be saved and unused here unless we need to
      // access the driver's outgoing directory for some reason.
      auto outgoing_dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
      ZX_ASSERT_MSG(!outgoing_dir_endpoints.is_error(), "Creating outgoing dir endpoints error: %s",
                    zx_status_get_string(outgoing_dir_endpoints.status_value()));
      outgoing_dir_client_ = fidl::WireSharedClient<fuchsia_io::Directory>(
          std::move(outgoing_dir_endpoints->client), host_dispatcher_);

      /* Prepare "ns" field in DriverStartArgs */

      // Create the outgoing directory for this driver host.
      host_outgoing_dir_ = std::make_unique<component::OutgoingDirectory>(host_dispatcher_);

      // Define service handler callback for parent driver.
      auto protocol = [this](fidl::ServerEnd<fuchsia_hardware_pci::Device> server_end) mutable {
        fidl::BindServer(host_dispatcher_, std::move(server_end), parent_server_.get());
      };

      // Register the callback to handler.
      fuchsia_hardware_pci::Service::InstanceHandler handler({.device = std::move(protocol)});

      // Fill the directory with necessary Protocols/Services
      ZX_ASSERT(host_outgoing_dir_->AddService<fuchsia_hardware_pci::Service>(std::move(handler))
                    .status_value() == ZX_OK);
      ZX_ASSERT(host_outgoing_dir_
                    ->AddUnmanagedProtocol<fuchsia_logger::LogSink>(
                        RouteToIncoming<fuchsia_logger::LogSink>())
                    .status_value() == ZX_OK);

      // Serve the directory to two entities: The hosted driver and DeviceServer.
      auto hosted_driver_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
      ZX_ASSERT(hosted_driver_endpoints.status_value() == ZX_OK);
      ZX_ASSERT(
          host_outgoing_dir_->Serve(std::move(hosted_driver_endpoints->server)).status_value() ==
          ZX_OK);

      // Create DeviceServer and it gets the ClientEnd to access the outgoing directory of driver
      // host here. Also it serves itself to the outgoing directory for the hosted driver to access.
      std::vector<std::string> service_offers;
      service_offers.push_back(ParentService::Name);

      // Create fake incoming namespace entries. Hosted driver gets its ClientEnd to access the
      // outgoing directory of driver host here.
      auto ns_entries = std::vector<fuchsia_component_runner::ComponentNamespaceEntry>();
      // Add the ClientEnd of the driver's
      ns_entries.emplace_back(fuchsia_component_runner::ComponentNamespaceEntry{{
          .path = "/",
          .directory = std::move(hosted_driver_endpoints->client),
      }});

      /* Fill DriverStartArgs with all the preparations*/
      start_args.node(std::move(node_endpoints->client))
          .incoming(std::move(ns_entries))
          .outgoing_dir(std::move(outgoing_dir_endpoints->server));

      setup_completion.Signal();
    };
    async::PostTask(host_dispatcher_, std::move(setup_task));
    zx_status_t status_wait = setup_completion.Wait(DISPATCHER_SYNC_TIMEOUT);
    if (status_wait == ZX_ERR_TIMED_OUT) {
      ZX_PANIC("Timeout waiting on host dispatcher.");
    }

    // Create/Start driver task on driver dispatcher.
    fdf::StartCompleter start_completer(
        [](void* ctx, zx_status_t status, void* driver) {
          auto* context = static_cast<StartCompleterContext*>(ctx);
          context->status = status;
          context->driver_ptr = static_cast<fdf::DriverBase*>(driver);
        },
        &context_);

    libsync::Completion start_completion;
    auto start_task = [this, args = std::move(start_args), &start_completion,
                       completer = std::move(start_completer)]() mutable {
      fdf_internal::BasicFactory<Driver>::CreateDriver(
          std::move(args), fdf::UnownedSynchronizedDispatcher(driver_dispatcher_.get()),
          std::move(completer));
      ZX_ASSERT_MSG(context_.status == ZX_OK, "Start driver failed: %d", context_.status);
      hosted_driver_.reset(context_.driver_ptr);
      start_completion.Signal();
    };

    async::PostTask(driver_dispatcher_.async_dispatcher(), std::move(start_task));
    status_wait = start_completion.Wait(DISPATCHER_SYNC_TIMEOUT);
    if (status_wait == ZX_ERR_TIMED_OUT) {
      ZX_PANIC("Timeout waiting on host dispatcher.");
    }
  }

  void PrepareStopDriver() {
    bool prepare_stop_called = false;

    auto completer = fdf::PrepareStopCompleter(PrepareStopCallback, &prepare_stop_called);
    ZX_ASSERT(hosted_driver_.get());
    hosted_driver_->PrepareStop(std::move(completer));
    ZX_ASSERT(prepare_stop_called);
  }

  void StopDriver() { hosted_driver_->Stop(); }

  // Give an option to wait for one async node removal.
  void WaitForNextNodeRemoval() {
    zx_status_t status = node_removal_completion_.Wait(NODE_REMOVAL_TIMEOUT);
    if (status == ZX_ERR_TIMED_OUT) {
      ZX_PANIC("Timeout waiting on node removal.");
    }
  }
  // Return the number of child nodes this driver creates.
  size_t ChildNodeCount() { return child_node_count_; }

  StartCompleterContext context_;
  std::unique_ptr<fdf::DriverBase> hosted_driver_;

 private:
  // The counter of hosted driver's child nodes.
  size_t child_node_count_ = 0;
  // The dispatcher to create driver on, and passed into the driver when creating it.
  fdf::Dispatcher driver_dispatcher_;

  async::Loop* node_loop_ = nullptr;
  // The dispatcher binds with the server end of fuchsia_driver_framework::Node protocol.
  async_dispatcher_t* host_dispatcher_ = nullptr;

  // The ClientEnd of hosted driver's own outgoing directory is saved here, not used.
  fidl::WireSharedClient<fuchsia_io::Directory> outgoing_dir_client_;

  // The outgoing directory of this fake driver host.
  std::unique_ptr<component::OutgoingDirectory> host_outgoing_dir_;

  libsync::Completion shutdown_completion_;
  libsync::Completion node_removal_completion_;

  // The parent protocol server implementation customized and provided by the tests which apply
  // SimDriverHost.
  std::unique_ptr<fidl::WireServer<ParentProtocol>> parent_server_;

  // We don't distinguish node now.
  std::vector<std::unique_ptr<SimNode>> node_list_;
};

}  // namespace wlan::simulation

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_TESTING_LIB_DFV2_SIM_DRIVER_HOST_H_
