// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_COMPONENT_INCOMING_CPP_SERVICE_WATCHER_H_
#define LIB_COMPONENT_INCOMING_CPP_SERVICE_WATCHER_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>

namespace component {

// A watcher for service instances.
//
// Watching is automatically stopped on destruction.
class ServiceWatcher final {
 public:
  // A callback to be invoked when service instances are added or removed.
  //
  // |event| will be either fuchsia_io::wire::WatchEvent::kExisting, if an instance was
  // existing at the beginning, fuchsia_io::wire::WatchEvent::kAdded, if an instance
  // was added, or fuchsia_io::wire::WatchEvent::kIdle, if all the existing instances
  // have been reported.
  // |instance| will be the name of the instance associated with the event.
  using Callback = fit::function<void(fuchsia_io::wire::WatchEvent event, std::string instance)>;

  // Begins watching for service instances in the service directory:
  // /svc/<service_name>. When a service is found, calls
  // the callback function with the instance name and the event type.
  // Begin will return ZX_ERR_UNAVAILABLE if it is called multiple times
  // without Cancel called first.
  zx_status_t Begin(std::string_view service_name, Callback callback,
                    async_dispatcher_t* dispatcher);

  // Same as above, but watches the provided directory.
  zx_status_t Begin(fidl::ClientEnd<fuchsia_io::Directory> dir, Callback callback,
                    async_dispatcher_t* dispatcher);

  // Cancels watching for service instances.
  zx_status_t Cancel();

  // used for tests to watch different directories
  void set_service_path(std::string path) { service_path_ = path; }

 private:
  void OnWatchedEvent(async_dispatcher_t* dispatcher, async::WaitBase* wait, zx_status_t status,
                      const zx_packet_signal_t* signal);

  Callback callback_;
  std::shared_ptr<std::array<uint8_t, fuchsia_io::wire::kMaxBuf>> buf_;
  zx::channel client_end_;
  std::string service_path_ = "/svc";
  async::WaitMethod<ServiceWatcher, &ServiceWatcher::OnWatchedEvent> wait_{this};
};

}  // namespace component

#endif  // LIB_COMPONENT_INCOMING_CPP_SERVICE_WATCHER_H_
