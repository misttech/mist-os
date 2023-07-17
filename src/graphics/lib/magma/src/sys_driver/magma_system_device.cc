// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_system_device.h"

#include <zircon/types.h>

#include "magma_system_connection.h"
#include "magma_util/macros.h"
#include "sys_driver/primary_fidl_server.h"

namespace msd {
uint32_t MagmaSystemDevice::GetDeviceId() {
  uint64_t result;
  magma::Status status = Query(MAGMA_QUERY_DEVICE_ID, &result);
  if (!status.ok())
    return 0;

  MAGMA_DASSERT(result >> 32 == 0);
  return static_cast<uint32_t>(result);
}

MagmaSystemDevice::~MagmaSystemDevice() {
  std::unique_lock<std::mutex> lock(connection_list_mutex_);
  MAGMA_DASSERT(!connection_set_ || connection_set_->empty());
}

std::unique_ptr<msd::PrimaryFidlServer> MagmaSystemDevice::Open(
    msd_client_id_t client_id, fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary,
    fidl::ServerEnd<fuchsia_gpu_magma::Notification> notification) {
  std::unique_ptr<msd::Connection> msd_connection = msd_dev()->Open(client_id);
  if (!msd_connection)
    return MAGMA_DRETP(nullptr, "msd_device_open failed");

  return msd::PrimaryFidlServer::Create(
      std::make_unique<MagmaSystemConnection>(this, std::move(msd_connection)), client_id,
      std::move(primary), std::move(notification));
}

void MagmaSystemDevice::StartConnectionThread(
    std::unique_ptr<msd::PrimaryFidlServer> fidl_server,
    fit::function<void(const char*)> set_thread_priority) {
  std::lock_guard<std::mutex> lock(connection_list_mutex_);
  auto server_holder = std::make_shared<PrimaryFidlServerHolder>();
  server_holder->Start(std::move(fidl_server), this, std::move(set_thread_priority));

  connection_set_->insert(std::move(server_holder));
}

void MagmaSystemDevice::ConnectionClosed(std::shared_ptr<PrimaryFidlServerHolder> server,
                                         bool* need_detach_out) {
  std::lock_guard<std::mutex> lock(connection_list_mutex_);

  // Connection is shutting down, thread will be joined on by MagmaSystemDevice::Shutdown.
  if (!connection_set_) {
    *need_detach_out = false;
    return;
  }

  auto iter = connection_set_->find(server);
  // Thread must be in the connection map, since Start was called in the same function that added it
  // (during a lock).
  MAGMA_DASSERT(iter != connection_set_->end());
  connection_set_->erase(iter);
  *need_detach_out = true;
}

void MagmaSystemDevice::Shutdown() {
  std::unique_ptr<std::unordered_set<std::shared_ptr<PrimaryFidlServerHolder>>> set;
  {
    std::lock_guard lock(connection_list_mutex_);
    set = std::move(connection_set_);
  }

  auto start = std::chrono::high_resolution_clock::now();
  for (auto& element : *set) {
    element->Shutdown();
  }
  // All threads should either be joined at this point, or waiting to detach.

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
      *result_out = msd::PrimaryFidlServer::kMaxInflightMessages;
      *result_out <<= 32;
      *result_out |= msd::PrimaryFidlServer::kMaxInflightMemoryMB;
      return MAGMA_STATUS_OK;
  }
  magma_status_t status = msd_dev()->Query(id, &vmo, result_out);
  if (result_buffer_out) {
    *result_buffer_out = vmo.release();
  }
  return status;
}

magma_status_t MagmaSystemDevice::GetIcdList(std::vector<MsdIcdInfo>* icd_list_out) {
  return msd_dev()->GetIcdList(icd_list_out);
}

}  // namespace msd
