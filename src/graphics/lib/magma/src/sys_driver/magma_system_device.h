// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_SYSTEM_DEVICE_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_SYSTEM_DEVICE_H_

#include <functional>
#include <memory>
#include <mutex>
#include <thread>
#include <unordered_map>
#include <vector>

#include "magma_system_connection.h"
#include "msd_cc.h"
#include "primary_fidl_server.h"

namespace msd {
class MagmaSystemBuffer;
class MagmaSystemSemaphore;

class MagmaSystemDevice {
 public:
  // The msd::Driver instance must outlive the MagmaSystemDevice
  static std::unique_ptr<MagmaSystemDevice> Create(msd::Driver* driver,
                                                   std::unique_ptr<msd::Device> msd_device) {
    return std::make_unique<MagmaSystemDevice>(driver, std::move(msd_device));
  }

  explicit MagmaSystemDevice(msd::Driver* driver, std::unique_ptr<msd::Device> msd_dev)
      : driver_(driver), msd_dev_(std::move(msd_dev)) {
    connection_map_ = std::make_unique<std::unordered_map<std::thread::id, Connection>>();
  }

  // Opens a connection to the device. On success, returns the connection handle
  // to be passed to the client.
  static std::shared_ptr<msd::PrimaryFidlServer> Open(
      std::shared_ptr<MagmaSystemDevice> device, msd_client_id_t client_id,
      fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary,
      fidl::ServerEnd<fuchsia_gpu_magma::Notification> notification);

  msd::Device* msd_dev() { return msd_dev_.get(); }
  msd::Driver* driver() { return driver_; }

  // Returns the device id. 0 is invalid.
  uint32_t GetDeviceId();

  // Called on driver thread
  void Shutdown();

  // Called on driver thread.  |device_handle| may be used by the connection thread for
  // initialization/configuration but should not be retained.
  void StartConnectionThread(std::shared_ptr<msd::PrimaryFidlServer> fidl_server,
                             fit::function<void(const char*)> set_thread_priority);

  // Called on connection thread
  void ConnectionClosed(std::thread::id thread_id);

  void DumpStatus(uint32_t dump_type) { msd_dev()->DumpStatus(dump_type); }

  magma::Status Query(uint64_t id, magma_handle_t* buffer_out, uint64_t* value_out);

  magma::Status Query(uint64_t id, uint64_t* value_out) { return Query(id, nullptr, value_out); }

  magma_status_t GetIcdList(std::vector<MsdIcdInfo>* icd_list_out);

  void SetMemoryPressureLevel(MagmaMemoryPressureLevel level);

  void set_perf_count_access_token_id(uint64_t id) { perf_count_access_token_id_ = id; }
  uint64_t perf_count_access_token_id() const { return perf_count_access_token_id_; }

 private:
  msd::Driver* driver_;
  std::unique_ptr<msd::Device> msd_dev_;
  uint64_t perf_count_access_token_id_ = 0u;

  struct Connection {
    std::thread thread;
    std::weak_ptr<msd::PrimaryFidlServer> server;
  };

  std::unique_ptr<std::unordered_map<std::thread::id, Connection>> connection_map_;
  std::mutex connection_list_mutex_;
};

}  // namespace msd

#endif  // SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_MAGMA_SYSTEM_DEVICE_H_
