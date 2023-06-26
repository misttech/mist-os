// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_CONNECTOR_LIB_DEVFS_FACTORY_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_CONNECTOR_LIB_DEVFS_FACTORY_H_

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/zx/result.h>

#include <cstdint>
#include <map>
#include <memory>

#include "src/lib/fsl/io/device_watcher.h"

namespace display {

// Implements the FIDL fuchsia.hardware.display.Provider API.  Only provides
// access to the coordinator as a primary client, but not a virtcon client.
//
// This class is thread unsafe. All FIDL calls must be dispatched onto the same
// dispatcher (i.e. the `dispatcher` argument in ctor) to which DeviceWatcher
// async tasks are dispatched.
class DevFsCoordinatorFactory : public fidl::Server<fuchsia_hardware_display::Provider> {
 public:
  // Creates a DevFsCoordinatorFactory which issues async tasks to `dispatcher`
  // and publishes the service to the `outgoing` directory.
  //
  // For thread safety, the `outgoing` directory must be also created using
  // `dispatcher`.
  static zx::result<> CreateAndPublishService(component::OutgoingDirectory& outgoing,
                                              async_dispatcher_t* dispatcher);

  // Creates a DevFsCoordinatorFactory which issues async tasks to `dispatcher`.
  explicit DevFsCoordinatorFactory(async_dispatcher_t* dispatcher);
  ~DevFsCoordinatorFactory() override = default;

  // Disallow copying, moving and assignment.
  DevFsCoordinatorFactory(const DevFsCoordinatorFactory&) = delete;
  DevFsCoordinatorFactory(DevFsCoordinatorFactory&&) = delete;
  DevFsCoordinatorFactory& operator=(const DevFsCoordinatorFactory&) = delete;
  DevFsCoordinatorFactory& operator=(DevFsCoordinatorFactory&&) = delete;

  // `fidl::Server<fuchsia_hardware_display::Provider>`
  void OpenCoordinatorForVirtcon(OpenCoordinatorForVirtconRequest& request,
                                 OpenCoordinatorForVirtconCompleter::Sync& completer) override;
  void OpenCoordinatorForPrimary(OpenCoordinatorForPrimaryRequest& request,
                                 OpenCoordinatorForPrimaryCompleter::Sync& completer) override;

 private:
  // Opens fuchsia.hardware.display.Coordinator service for a primary client
  // at service located at "dir/filename" using the server end channel
  // `coordinator_server`.
  static zx_status_t OpenCoordinatorForPrimaryOnDevice(
      const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename,
      fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server);

  // The dispatcher where all async tasks are dispatched onto by the
  // `DevFsCoordinatorFactory`.
  async_dispatcher_t* dispatcher_;

  // The currently outstanding DeviceWatcher closures.  The closures will remove
  // themselves from here if they are invoked before shutdown.  Any closures
  // still outstanding will be handled by the destructor.
  std::map<int64_t, std::unique_ptr<fsl::DeviceWatcher>> pending_device_watchers_;

  // ID of the next connected display client to identify queued display
  // coordinator client connections.
  int64_t next_display_client_id_ = 1;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_CONNECTOR_LIB_DEVFS_FACTORY_H_
