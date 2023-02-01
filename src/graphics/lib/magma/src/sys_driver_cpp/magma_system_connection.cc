// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "magma_system_connection.h"

#include <vector>

#include "magma_system_device.h"
#include "magma_util/macros.h"

MagmaSystemConnection::MagmaSystemConnection(std::weak_ptr<MagmaSystemDevice> weak_device,
                                             std::unique_ptr<msd::Connection> msd_connection_t)
    : device_(weak_device), msd_connection_(std::move(msd_connection_t)) {
  DASSERT(msd_connection_);
}

MagmaSystemConnection::~MagmaSystemConnection() {
  // Remove all contexts before clearing buffers, to give the hardware driver an
  // indication that faults afterwards may be due to buffer mappings having gone
  // away due to the shutdown.
  context_map_.clear();
  for (auto iter = buffer_map_.begin(); iter != buffer_map_.end();) {
    msd_connection()->ReleaseBuffer(*iter->second.buffer->msd_buf());
    iter = buffer_map_.erase(iter);
  }

  // Iterating over pool_map_ without the mutex held is safe because the map is only modified from
  // this thread.
  for (auto& pool_map_entry : pool_map_) {
    msd_connection()->ReleasePerformanceCounterBufferPool(
        std::move(pool_map_entry.second.msd_pool));
  }
  {
    // We still need to lock the mutex before modifying the map.
    std::lock_guard<std::mutex> lock(pool_map_mutex_);
    pool_map_.clear();
  }
  // Reset all MSD objects before calling ConnectionClosed() because the msd device might go away
  // any time after ConnectionClosed() and we don't want any dangling dependencies.
  semaphore_map_.clear();
  msd_connection_.reset();

  auto device = device_.lock();
  if (device) {
    device->ConnectionClosed(std::this_thread::get_id());
  }
}

uint32_t MagmaSystemConnection::GetDeviceId() {
  auto device = device_.lock();
  return device ? device->GetDeviceId() : 0;
}

magma::Status MagmaSystemConnection::CreateContext(uint32_t context_id) {
  auto iter = context_map_.find(context_id);
  if (iter != context_map_.end())
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Attempting to add context with duplicate id");

  auto msd_ctx = msd_connection_->CreateContext();
  if (!msd_ctx)
    return DRET_MSG(MAGMA_STATUS_INTERNAL_ERROR, "Failed to create msd context");

  auto ctx = std::unique_ptr<MagmaSystemContext>(new MagmaSystemContext(this, std::move(msd_ctx)));

  context_map_.insert(std::make_pair(context_id, std::move(ctx)));
  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::DestroyContext(uint32_t context_id) {
  auto iter = context_map_.find(context_id);
  if (iter == context_map_.end())
    return DRETF(MAGMA_STATUS_INVALID_ARGS,
                 "MagmaSystemConnection:Attempting to destroy invalid context id");

  context_map_.erase(iter);
  return MAGMA_STATUS_OK;
}

MagmaSystemContext* MagmaSystemConnection::LookupContext(uint32_t context_id) {
  auto iter = context_map_.find(context_id);
  if (iter == context_map_.end())
    return DRETP(nullptr, "MagmaSystemConnection: Attempting to lookup invalid context id");

  return iter->second.get();
}

magma::Status MagmaSystemConnection::ExecuteCommandBufferWithResources(
    uint32_t context_id, std::unique_ptr<magma_command_buffer> command_buffer,
    std::vector<magma_exec_resource> resources, std::vector<uint64_t> semaphores) {
  auto context = LookupContext(context_id);
  if (!context)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS,
                    "Attempting to execute command buffer on invalid context");

  return context->ExecuteCommandBufferWithResources(std::move(command_buffer), std::move(resources),
                                                    std::move(semaphores));
}

magma::Status MagmaSystemConnection::ExecuteImmediateCommands(uint32_t context_id,
                                                              uint64_t commands_size,
                                                              void* commands,
                                                              uint64_t semaphore_count,
                                                              uint64_t* semaphore_ids) {
  auto context = LookupContext(context_id);
  if (!context)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS,
                    "Attempting to execute command buffer on invalid context");

  return context->ExecuteImmediateCommands(commands_size, commands, semaphore_count, semaphore_ids);
}

magma::Status MagmaSystemConnection::EnablePerformanceCounterAccess(
    std::unique_ptr<magma::PlatformHandle> access_token) {
  auto device = device_.lock();
  if (!device) {
    return DRET(MAGMA_STATUS_INTERNAL_ERROR);
  }
  uint64_t perf_count_access_token_id = device->perf_count_access_token_id();
  DASSERT(perf_count_access_token_id);
  if (!access_token) {
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  }
  if (access_token->global_id() != perf_count_access_token_id) {
    // This is not counted as an error, since it can happen if the client uses the event from the
    // wrong driver.
    return MAGMA_STATUS_OK;
  }

  DLOG("Performance counter access enabled");
  can_access_performance_counters_ = true;
  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::ImportBuffer(uint32_t handle, uint64_t id) {
  auto device = device_.lock();
  if (!device) {
    return DRET(MAGMA_STATUS_INTERNAL_ERROR);
  }
  auto buffer = magma::PlatformBuffer::Import(handle);
  if (!buffer)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "failed to import buffer");

  buffer->set_local_id(id);

  auto iter = buffer_map_.find(id);
  if (iter != buffer_map_.end()) {
    iter->second.refcount++;
    return MAGMA_STATUS_OK;
  }

  BufferReference ref;
  ref.buffer = MagmaSystemBuffer::Create(device->driver(), std::move(buffer));
  buffer_map_.insert({id, ref});

  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::ReleaseBuffer(uint64_t id) {
  auto iter = buffer_map_.find(id);
  if (iter == buffer_map_.end())
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Attempting to free invalid buffer id %lu", id);

  if (--iter->second.refcount > 0)
    return MAGMA_STATUS_OK;

  msd_connection()->ReleaseBuffer(*iter->second.buffer->msd_buf());
  buffer_map_.erase(iter);

  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::MapBuffer(uint64_t id, uint64_t hw_va, uint64_t offset,
                                               uint64_t length, uint64_t flags) {
  auto iter = buffer_map_.find(id);
  if (iter == buffer_map_.end())
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Attempting to map invalid buffer id %lu", id);

  if (length + offset < length)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Offset overflows");

  if (length + offset > iter->second.buffer->size())
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Offset + length too large for buffer");

  if (!flags)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Flags must be nonzero");

  magma::Status status =
      msd_connection()->MapBuffer(*iter->second.buffer->msd_buf(), hw_va, offset, length, flags);
  if (!status.ok())
    return DRET_MSG(status.get(), "msd_connection_map_buffer failed");

  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::UnmapBuffer(uint64_t id, uint64_t hw_va) {
  auto iter = buffer_map_.find(id);
  if (iter == buffer_map_.end())
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Attempting to unmap invalid buffer id");

  magma::Status status = msd_connection()->UnmapBuffer(*iter->second.buffer->msd_buf(), hw_va);
  if (!status.ok())
    return DRET_MSG(status.get(), "msd_connection_unmap_buffer failed");

  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::BufferRangeOp(uint64_t id, uint32_t op, uint64_t start,
                                                   uint64_t length) {
  auto iter = buffer_map_.find(id);
  if (iter == buffer_map_.end())
    return DRETF(false, "Attempting to commit invalid buffer id");
  if (start + length < start) {
    return DRETF(false, "Offset overflows");
  }
  if (start + length > iter->second.buffer->size()) {
    return DRETF(false, "Page offset too large for buffer");
  }
  return msd_connection()->BufferRangeOp(*iter->second.buffer->msd_buf(), op, start, length);
}

void MagmaSystemConnection::SetNotificationCallback(
    msd::NotificationHandler* notification_handler) {
  if (notification_handler) {
    notification_handler_ = notification_handler;
    msd_connection()->SetNotificationCallback(this);
  } else {
    msd_connection()->SetNotificationCallback(nullptr);
  }
}

void MagmaSystemConnection::NotificationChannelSend(cpp20::span<uint8_t> data) {
  DASSERT(notification_handler_);
  notification_handler_->NotificationChannelSend(data);
}

void MagmaSystemConnection::ContextKilled() {
  DASSERT(notification_handler_);
  notification_handler_->ContextKilled();
}
void MagmaSystemConnection::PerformanceCounterReadCompleted(const msd::PerfCounterResult& result) {
  DASSERT(notification_handler_);
  std::lock_guard<std::mutex> lock(pool_map_mutex_);

  auto pool_it = pool_map_.find(result.pool_id);
  if (pool_it == pool_map_.end()) {
    DLOG("Driver attempted to lookup deleted pool id %ld\n", result.pool_id);
    return;
  }

  pool_it->second.platform_pool->SendPerformanceCounterCompletion(
      result.trigger_id, result.buffer_id, result.buffer_offset, result.timestamp,
      result.result_flags);
}

void MagmaSystemConnection::HandleWait(msd_connection_handle_wait_start_t starter,
                                       msd_connection_handle_wait_complete_t completer,
                                       void* wait_context, zx::unowned_handle handle) {
  DASSERT(notification_handler_);
  notification_handler_->HandleWait(starter, completer, wait_context, std::move(handle));
}
void MagmaSystemConnection::HandleWaitCancel(void* cancel_token) {
  DASSERT(notification_handler_);
  notification_handler_->HandleWaitCancel(cancel_token);
}

async_dispatcher_t* MagmaSystemConnection::GetAsyncDispatcher() {
  DASSERT(notification_handler_);
  return notification_handler_->GetAsyncDispatcher();
}

magma::Status MagmaSystemConnection::ImportObject(uint32_t handle,
                                                  magma::PlatformObject::Type object_type,
                                                  uint64_t client_id) {
  if (!client_id)
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "client_id must be non zero");

  auto device = device_.lock();
  if (!device)
    return DRET_MSG(MAGMA_STATUS_INTERNAL_ERROR, "failed to lock device");

  switch (object_type) {
    case magma::PlatformObject::BUFFER:
      return ImportBuffer(handle, client_id);

    case magma::PlatformObject::SEMAPHORE: {
      // Always import the handle to ensure it gets closed
      auto platform_sem = magma::PlatformSemaphore::Import(handle);
      if (!platform_sem)
        return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "failed to import platform semaphore");

      platform_sem->set_local_id(client_id);

      auto iter = semaphore_map_.find(client_id);
      if (iter != semaphore_map_.end()) {
        iter->second.refcount++;
        return MAGMA_STATUS_OK;
      }

      auto semaphore = MagmaSystemSemaphore::Create(device->driver(), std::move(platform_sem));
      DASSERT(semaphore);

      SemaphoreReference ref;
      ref.semaphore = std::move(semaphore);
      semaphore_map_.insert(std::make_pair(client_id, ref));
    } break;
    default:
      return DRET(MAGMA_STATUS_INVALID_ARGS);
  }

  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::ReleaseObject(uint64_t object_id,
                                                   magma::PlatformObject::Type object_type) {
  switch (object_type) {
    case magma::PlatformObject::BUFFER:
      return ReleaseBuffer(object_id);

    case magma::PlatformObject::SEMAPHORE: {
      auto iter = semaphore_map_.find(object_id);
      if (iter == semaphore_map_.end())
        return DRET_MSG(MAGMA_STATUS_INVALID_ARGS,
                        "Attempting to release invalid semaphore id 0x%" PRIx64, object_id);

      if (--iter->second.refcount > 0)
        return MAGMA_STATUS_OK;

      semaphore_map_.erase(iter);
    } break;
    default:
      return DRET(MAGMA_STATUS_INVALID_ARGS);
  }
  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::EnablePerformanceCounters(const uint64_t* counters,
                                                               uint64_t counter_count) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);

  return msd_connection()->EnablePerformanceCounters(cpp20::span(counters, counter_count));
}

magma::Status MagmaSystemConnection::CreatePerformanceCounterBufferPool(
    std::unique_ptr<magma::PlatformPerfCountPool> pool) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);

  uint64_t pool_id = pool->pool_id();
  if (pool_map_.count(pool_id))
    return DRET(MAGMA_STATUS_INVALID_ARGS);

  {
    std::lock_guard<std::mutex> lock(pool_map_mutex_);
    pool_map_[pool_id].platform_pool = std::move(pool);
  }
  // |pool_map_mutex_| is unlocked before calling into the driver to prevent deadlocks if the driver
  // synchronously does MSD_CONNECTION_NOTIFICATION_PERFORMANCE_COUNTERS_READ_COMPLETED.
  magma_status_t status =
      msd_connection()->CreatePerformanceCounterBufferPool(pool_id, &pool_map_[pool_id].msd_pool);
  if (status != MAGMA_STATUS_OK) {
    std::lock_guard<std::mutex> lock(pool_map_mutex_);
    pool_map_.erase(pool_id);
  }
  return MAGMA_STATUS_OK;
}

magma::Status MagmaSystemConnection::ReleasePerformanceCounterBufferPool(uint64_t pool_id) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);

  auto it = pool_map_.find(pool_id);
  if (it == pool_map_.end())
    return DRET_MSG(MAGMA_STATUS_INVALID_ARGS, "Invalid pool id %ld", pool_id);
  std::unique_ptr<msd::PerfCountPool>& msd_pool = it->second.msd_pool;

  // |pool_map_mutex_| is unlocked before calling into the driver to prevent deadlocks if the driver
  // synchronously does MSD_CONNECTION_NOTIFICATION_PERFORMANCE_COUNTERS_READ_COMPLETED.
  magma_status_t status =
      msd_connection()->ReleasePerformanceCounterBufferPool(std::move(msd_pool));
  {
    std::lock_guard<std::mutex> lock(pool_map_mutex_);
    pool_map_.erase(pool_id);
  }
  return DRET(status);
}

magma::Status MagmaSystemConnection::AddPerformanceCounterBufferOffsetToPool(uint64_t pool_id,
                                                                             uint64_t buffer_id,
                                                                             uint64_t buffer_offset,
                                                                             uint64_t buffer_size) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);
  std::shared_ptr<MagmaSystemBuffer> buffer = LookupBuffer(buffer_id);
  if (!buffer) {
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  }
  msd::PerfCountPool* msd_pool = LookupPerfCountPool(pool_id);
  if (!msd_pool) {
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  }
  magma_status_t status = msd_connection()->AddPerformanceCounterBufferOffsetToPool(
      *msd_pool, *buffer->msd_buf(), buffer_id, buffer_offset, buffer_size);
  return DRET(status);
}

magma::Status MagmaSystemConnection::RemovePerformanceCounterBufferFromPool(uint64_t pool_id,
                                                                            uint64_t buffer_id) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);
  std::shared_ptr<MagmaSystemBuffer> buffer = LookupBuffer(buffer_id);
  if (!buffer) {
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  }

  msd::PerfCountPool* msd_pool = LookupPerfCountPool(pool_id);
  if (!msd_pool)
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  magma_status_t status =
      msd_connection()->RemovePerformanceCounterBufferFromPool(*msd_pool, *buffer->msd_buf());

  return DRET(status);
}

magma::Status MagmaSystemConnection::DumpPerformanceCounters(uint64_t pool_id,
                                                             uint32_t trigger_id) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);
  msd::PerfCountPool* msd_pool = LookupPerfCountPool(pool_id);
  if (!msd_pool)
    return DRET(MAGMA_STATUS_INVALID_ARGS);
  return msd_connection()->DumpPerformanceCounters(*msd_pool, trigger_id);
}

magma::Status MagmaSystemConnection::ClearPerformanceCounters(const uint64_t* counters,
                                                              uint64_t counter_count) {
  if (!can_access_performance_counters_)
    return DRET(MAGMA_STATUS_ACCESS_DENIED);
  return msd_connection()->ClearPerformanceCounters(cpp20::span(counters, counter_count));
}

std::shared_ptr<MagmaSystemBuffer> MagmaSystemConnection::LookupBuffer(uint64_t id) {
  auto iter = buffer_map_.find(id);
  if (iter == buffer_map_.end())
    return DRETP(nullptr, "Attempting to lookup invalid buffer id");

  return iter->second.buffer;
}

std::shared_ptr<MagmaSystemSemaphore> MagmaSystemConnection::LookupSemaphore(uint64_t id) {
  auto iter = semaphore_map_.find(id);
  if (iter == semaphore_map_.end())
    return nullptr;
  return iter->second.semaphore;
}

msd::PerfCountPool* MagmaSystemConnection::LookupPerfCountPool(uint64_t id) {
  auto it = pool_map_.find(id);
  if (it == pool_map_.end())
    return DRETP(nullptr, "Invalid pool id %ld", id);
  return it->second.msd_pool.get();
}
