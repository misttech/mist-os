// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_COMPONENT_INCOMING_CPP_SERVICE_MEMBER_WATCHER_H_
#define LIB_COMPONENT_INCOMING_CPP_SERVICE_MEMBER_WATCHER_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.unknown/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/constants.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/component/incoming/cpp/service_watcher.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>

#include <deque>
#include <type_traits>
#include <utility>

namespace component {

// Watch for service instances and connect to a member protocol of each instance.
//
// ServiceMemberWatcher and SyncServiceMemberWatcher are templated on a ServiceMember, which
// specifies both the service that it is a part of, and a member protocol of that service.
//
// For a fidl protocol and service defined as:
//   library fidl.examples.echo;
//   protocol DriverEcho {...}
//   service DriverEchoService {
//       echo_device client_end:DriverEcho;
//   };
//
// The ServiceMember would be: fidl_examples_echo::Service::EchoDevice
// Note that the last part of the ServiceMember corresponds to the name of the
// client_end in the service, not the protocol type.
//
// Services can be waited on asynchronously with ServiceMemberWatcher and synchronously with
// SyncServiceMemberWatcher.  If you have a service with multiple protocols, and need to access
// more than one protocol of a service for each instance, you can use component::ServiceWatcher
//
// Define a callback function:
//  void OnInstanceFound(ClientEnd<fidl_examples_echo::DriverEcho> client_end) {...}
// Optionally define an idle function, which will be called when all existing instances have been
// enumerated:
//  void AllExistingEnumerated() {...}
// Create the ServiceMemberWatcher:
// ServiceMemberWatcher<fidl_examples_echo::Service::EchoDevice> watcher;
// watcher->Begin(get_default_dispatcher(), &OnInstanceFound, &AllExistingEnumerated);
//
// The ServiceMemberWatcher will stop upon destruction, or when |Cancel| is called.
template <typename ServiceMember>
class ServiceMemberWatcher {
  static_assert(fidl::IsServiceMemberV<ServiceMember>, "Type must be a member of a service");

 public:
  using Protocol = typename ServiceMember::ProtocolType;
  using ClientCallback = fit::function<void(fidl::ClientEnd<Protocol>)>;
  // Callback function which is invoked after the existing service instances have been
  // reported via ClientCallback, and before newly-arriving service instances are delivered
  // via ClientCallback.
  using IdleCallback = fit::callback<void()>;

  // Cancels watching for service instances.
  zx::result<> Cancel() {
    client_callback_ = nullptr;
    return zx::make_result(service_watcher_.Cancel());
  }

  ServiceMemberWatcher() = default;
  // Begins asynchronously waiting for service instances using the given dispatcher.
  //
  // Waits for services in the incoming service directory: /svc/ServiceMember::ServiceName
  //
  // Asynchronously invokes |client_callback| for all existing service instances
  // within the specified aggregate service type, as any subsequently added
  // devices until service member watcher is destroyed. Ignores any service
  // named ".".
  //
  // The |idle_callback| is invoked once immediately after all pre-existing
  // service instances have been reported via |client_callback| shortly after creation.
  // After |idle_callback| returns, any newly-arriving devices are reported via
  // |client_callback|.
  // |idle_callback| will be deleted after it is called, so captured context
  // is guaranteed to not be retained.
  zx::result<> Begin(
      async_dispatcher_t* dispatcher, ClientCallback callback, IdleCallback idle_callback = [] {}) {
    // Begin should only be called once
    ZX_ASSERT(client_callback_ == nullptr);
    client_callback_ = std::move(callback);
    idle_callback_ = std::move(idle_callback);
    return zx::make_result(service_watcher_.Begin(
        ServiceMember::ServiceName, fit::bind_member<&ServiceMemberWatcher::OnWatchedEvent>(this),
        dispatcher));
  }

 private:
  void OnWatchedEvent(fuchsia_io::wire::WatchEvent event, std::string instance) {
    if (event == fuchsia_io::wire::WatchEvent::kIdle) {
      idle_callback_();
      return;
    }
    if (event == fuchsia_io::wire::WatchEvent::kRemoved || instance.size() < 2) {
      return;
    }
    auto svc_dir = OpenServiceRoot();
    ZX_ASSERT(svc_dir.is_ok());

    zx::result<fidl::ClientEnd<typename ServiceMember::ProtocolType>> client_result =
        ConnectAtMember<ServiceMember>(svc_dir.value(), instance);
    // This should not fail, since the directory just gave us the instance.
    ZX_ASSERT(client_result.is_ok());
    client_callback_(std::move(client_result.value()));
  }

  ClientCallback client_callback_;
  IdleCallback idle_callback_;
  component::ServiceWatcher service_watcher_;
};

// SyncServiceMemberWatcher allows services to be waited for synchronously.
// Note that the this class is templated on the service member name, not the service name.
// For example:
// SyncServiceMemberWatcher<fidl_examples_echo::Service::EchoDevice> watcher;
// zx::result<ClientEnd<fidl_examples_echo::DriverEcho>> result = watcher.GetNextInstance(true);
template <typename ServiceMember>
class SyncServiceMemberWatcher final : public ServiceMemberWatcher<ServiceMember> {
  static_assert(fidl::IsServiceMemberV<ServiceMember>, "Type must be a member of a service");

 public:
  using Protocol = typename ServiceMember::ProtocolType;
  SyncServiceMemberWatcher() = default;

  // Sequentially query for service instances at /svc/ServiceMember::ServiceName
  //
  // This call will block until a service instance is found. When an instance of the given
  // service is detected in the /svc/ServiceMember::ServiceName directory, this function
  // will return a ClientEnd to the protocol specified by ServiceMember::ProtocolType.
  //
  // Subsequent calls to GetNextInstance will return other instances if they exist.
  // GetNextInstance will iterate through all service instances of a given type.
  // When all of the existing service instances have been returned,
  // if |stop_at_idle| is true, GetNextInstance will return a zx::error(ZX_ERR_STOP).
  // Otherwise, GetNextInstance will wait until |deadline| for a new instance to appear.
  zx::result<fidl::ClientEnd<Protocol>> GetNextInstance(bool stop_at_idle,
                                                        zx::time deadline = zx::time::infinite()) {
    if (!has_begun_iterating_) {
      zx::result result = this->Begin(
          loop_.dispatcher(),
          [this](fidl::ClientEnd<Protocol> client_in) {
            clients_.emplace_back(std::move(client_in));
          },
          [this]() { idle_called_ = true; });
      if (result.is_error()) {
        return result.take_error();
      }
      has_begun_iterating_ = true;
    }
    // Run the loop to get the file events
    // Due to the nature of the fuchsia_io::Watcher protocol, one event for the service watcher may
    // correspond to multiple file events.  For this reason, we just run the loop until idle and
    // then process all the file events that we have received.
    zx_status_t run_status;
    do {
      run_status = loop_.RunUntilIdle();
      if (run_status != ZX_OK) {  // loop was cancelled or shutdown
        return zx::error(run_status);
      }
      // First get all the entries that were existing/added:
      if (!clients_.empty()) {
        fidl::ClientEnd<Protocol> client = std::move(clients_.front());
        clients_.pop_front();
        return zx::ok(std::move(client));
      }
      // Once the queue is emptied, we can let the user know that the idle callback was invoked.
      if (stop_at_idle && idle_called_) {
        return zx::error(ZX_ERR_STOP);
      }
      // At this point, we are either in a race for the idle signal, or stop_at_idle == false,
      // and we are just waiting for any future file events.
      run_status = loop_.Run(deadline, true);
    } while (run_status == ZX_OK);
    // loop_.Run exited with a timeout or because it was canceled.
    return zx::error(run_status);
  }

 private:
  bool has_begun_iterating_ = false;
  bool idle_called_ = false;

  // For doing blocking waits:
  std::deque<fidl::ClientEnd<Protocol>> clients_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

}  // namespace component

#endif  // LIB_COMPONENT_INCOMING_CPP_SERVICE_MEMBER_WATCHER_H_
