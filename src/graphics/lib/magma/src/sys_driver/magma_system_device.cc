// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_system_device.h"

#include <zircon/types.h>

#include "magma_system_connection.h"
#include "magma_util/macros.h"

namespace msd {
uint32_t MagmaSystemDevice::GetDeviceId() {
  uint64_t result;
  magma::Status status = Query(MAGMA_QUERY_DEVICE_ID, &result);
  if (!status.ok())
    return 0;

  MAGMA_DASSERT(result >> 32 == 0);
  return static_cast<uint32_t>(result);
}

std::shared_ptr<msd::ZirconConnection> MagmaSystemDevice::Open(
    std::shared_ptr<MagmaSystemDevice> device, msd_client_id_t client_id,
    fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary,
    fidl::ServerEnd<fuchsia_gpu_magma::Notification> notification) {
  std::unique_ptr<msd::Connection> msd_connection = device->msd_dev()->Open(client_id);
  if (!msd_connection)
    return MAGMA_DRETP(nullptr, "msd_device_open failed");

  return msd::ZirconConnection::Create(
      std::make_unique<MagmaSystemConnection>(std::move(device), std::move(msd_connection)),
      client_id, std::move(primary), std::move(notification));
}

void MagmaSystemDevice::StartConnectionThread(
    std::shared_ptr<msd::ZirconConnection> platform_connection,
    fit::function<void(const char*)> set_thread_priority) {
  std::unique_lock<std::mutex> lock(connection_list_mutex_);

  std::thread thread(msd::ZirconConnection::RunLoop, platform_connection,
                     std::move(set_thread_priority));

  connection_map_->insert(std::pair<std::thread::id, Connection>(
      thread.get_id(), Connection{std::move(thread), platform_connection}));
}

void MagmaSystemDevice::ConnectionClosed(std::thread::id thread_id) {
  std::unique_lock<std::mutex> lock(connection_list_mutex_);

  if (!connection_map_)
    return;

  auto iter = connection_map_->find(thread_id);
  // May not be in the map if no connection thread was started.
  if (iter != connection_map_->end()) {
    iter->second.thread.detach();
    connection_map_->erase(iter);
  }
}

void MagmaSystemDevice::Shutdown() {
  std::unique_lock<std::mutex> lock(connection_list_mutex_);
  auto map = std::move(connection_map_);
  lock.unlock();

  for (auto& element : *map) {
    auto locked = element.second.connection.lock();
    if (locked) {
      locked->Shutdown();
    }
  }

  auto start = std::chrono::high_resolution_clock::now();

  for (auto& element : *map) {
    element.second.thread.join();
  }

  std::chrono::duration<double, std::milli> elapsed =
      std::chrono::high_resolution_clock::now() - start;
  MAGMA_DLOG("shutdown took %u ms", (uint32_t)elapsed.count());

  (void)elapsed;
}

void MagmaSystemDevice::SetMemoryPressureLevel(MagmaMemoryPressureLevel level) {
  msd_dev()->SetMemoryPressureLevel(level);
}

magma::Status MagmaSystemDevice::Query(uint64_t id, magma_handle_t* result_buffer_out,
                                       uint64_t* result_out) {
  zx::vmo vmo;
  switch (id) {
    case MAGMA_QUERY_MAXIMUM_INFLIGHT_PARAMS:
      *result_out = msd::ZirconConnection::kMaxInflightMessages;
      *result_out <<= 32;
      *result_out |= msd::ZirconConnection::kMaxInflightMemoryMB;
      return MAGMA_STATUS_OK;
  }
  magma_status_t status = msd_dev()->Query(id, &vmo, result_out);
  if (result_buffer_out) {
    *result_buffer_out = vmo.release();
  }
  return status;
}

magma_status_t MagmaSystemDevice::GetIcdList(std::vector<msd_icd_info_t>* icd_list_out) {
  return msd_dev()->GetIcdList(icd_list_out);
}

}  // namespace msd
