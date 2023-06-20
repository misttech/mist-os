// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_LIB_MAGMA_INCLUDE_MSD_MSD_CC_H_
#define SRC_GRAPHICS_LIB_MAGMA_INCLUDE_MSD_MSD_CC_H_

#include <lib/async/dispatcher.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/event.h>
#include <lib/zx/vmo.h>

#include <memory>

#include "msd_defs.h"

namespace msd {

class Connection;
class Device;
class Semaphore;
class Buffer;
class Context;

// This struct represents all the information about the device that the driver needs to interact
// with it. The implementation of this struct is driver-specific.
struct DeviceHandle;

// This represents the driver for a device. It's a singleton that can't access device registers.
class Driver {
 public:
  static std::unique_ptr<Driver> Create();

  virtual ~Driver() = 0;

  // Configures the driver according to |flags|.
  virtual void Configure(uint32_t flags) {}
  // Returns a buffer handle that contains inspect data for the driver.  Returns
  // ZX_HANDLE_INVALID if driver doesn't support inspect.
  virtual std::optional<inspect::Inspector> DuplicateInspector() { return std::nullopt; }

  // Creates a device at system startup. `device_data` is a pointer to a platform-specific device
  // object which is guaranteed to outlive the returned Device.
  virtual std::unique_ptr<Device> CreateDevice(DeviceHandle* device_data) { return {}; }

  // Creates a buffer that owns the provided handle. Can be called on any thread.
  virtual std::unique_ptr<Buffer> ImportBuffer(zx::vmo vmo, uint64_t client_id) { return {}; }

  // Creates a semaphore that owns the provided handle. Can be called on any thread.
  virtual magma_status_t ImportSemaphore(zx::event handle, uint64_t client_id,
                                         std::unique_ptr<Semaphore>* out) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }
};

// This represents a single hardware device. Unless otherwise specified, all calls into this class
// are serialized.
class Device {
 public:
  virtual ~Device() = 0;

  // Signals the current memory pressure level for the system. May be called on any thread.
  virtual void SetMemoryPressureLevel(MagmaMemoryPressureLevel level) {}

  // Returns a value associated with the given id. On MAGMA_STATUS_OK, a given query `id` will
  // return either a buffer in `result_buffer_out`, or a value in `result_out`. nullptr may be
  // provided for whichever result parameter is not needed.
  virtual magma_status_t Query(uint64_t id, zx::vmo* result_buffer_out, uint64_t* result_out) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  // Outputs a list of ICD components.
  virtual magma_status_t GetIcdList(std::vector<MsdIcdInfo>* icd_info_out) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  virtual void DumpStatus(uint32_t dump_flags) {}

  // Opens a device for the given client. Returns nullptr on failure
  virtual std::unique_ptr<Connection> Open(msd_client_id_t client_id) { return {}; }
};

struct PerfCounterResult {
  uint64_t pool_id;
  uint32_t trigger_id;
  uint64_t buffer_id;
  uint32_t buffer_offset;
  uint64_t timestamp;
  uint32_t result_flags;
};

// Implemented by sys_driver. This class is used to call into sys_driver to notify it of operations
// it needs to handle.
class NotificationHandler {
 public:
  virtual void NotificationChannelSend(cpp20::span<uint8_t> data) = 0;
  virtual void ContextKilled() = 0;
  virtual void PerformanceCounterReadCompleted(const PerfCounterResult& result) = 0;
  // Returns an async_dispatcher_t that runs commands serialized with this
  // Connection. This dispatcher will be shut down before the Connection is
  // destroyed.
  virtual async_dispatcher_t* GetAsyncDispatcher() = 0;
};

class PerfCountPool {
 public:
  virtual ~PerfCountPool() = 0;
};

// This represents a single connection from a client. All method calls on this class are serialized.
class Connection {
 public:
  virtual ~Connection() = 0;
  virtual magma_status_t MapBuffer(Buffer& buffer, uint64_t gpu_va, uint64_t offset,
                                   uint64_t length, uint64_t flags) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }
  virtual magma_status_t UnmapBuffer(Buffer& buffer, uint64_t gpu_va) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }
  virtual magma_status_t BufferRangeOp(Buffer& buffer, uint32_t options, uint64_t start_offset,
                                       uint64_t length) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }
  // Signals that the given |buffer| is no longer in use on the given |connection|. This must be
  // called for every connection associated with a buffer before the buffer is destroyed, or for
  // every buffer associated with a connection before the connection is destroyed.
  virtual void ReleaseBuffer(Buffer& buffer) {}

  // Sets the callback to be used by a connection for various notifications.
  // This is called when a connection is created, and also called to unset
  // the callback before a connection is destroyed.  A multithreaded
  // implementation must be careful to guard use of this callback to avoid
  // collision with possible concurrent destruction.
  virtual void SetNotificationCallback(NotificationHandler* handler) {}
  virtual std::unique_ptr<Context> CreateContext() { return {}; }

  virtual magma_status_t EnablePerformanceCounters(cpp20::span<const uint64_t> counters) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  virtual magma_status_t CreatePerformanceCounterBufferPool(
      uint64_t pool_id, std::unique_ptr<PerfCountPool>* pool_out) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  // Releases the performance counter buffer pool. This driver must not send any notification with
  // the pool ID of this pool after it returns from this method.
  virtual magma_status_t ReleasePerformanceCounterBufferPool(std::unique_ptr<PerfCountPool> pool) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  virtual magma_status_t AddPerformanceCounterBufferOffsetToPool(PerfCountPool& pool,
                                                                 Buffer& buffer, uint64_t buffer_id,
                                                                 uint64_t buffer_offset,
                                                                 uint64_t buffer_size) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  virtual magma_status_t RemovePerformanceCounterBufferFromPool(PerfCountPool& pool,
                                                                Buffer& buffer) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  virtual magma_status_t DumpPerformanceCounters(PerfCountPool& pool, uint32_t trigger_id) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  virtual magma_status_t ClearPerformanceCounters(cpp20::span<const uint64_t> counters) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }
};

// This represents a single hardware context that may execute commands.
class Context {
 public:
  virtual ~Context() = 0;

  // Executes a command buffer given associated set of resources and semaphores.
  // |command_buffer| is the command buffer to be executed
  // |exec_resources| describe the associated resources
  // |buffers| are the buffers referenced the ids in |exec_resource|, in the same order
  // |wait_semaphores| are the semaphores that must be signaled before starting command buffer
  // execution
  // |signal_semaphores| are the semaphores to be signaled upon completion of the command buffer
  virtual magma_status_t ExecuteCommandBufferWithResources(magma_command_buffer* command_buffer,
                                                           magma_exec_resource* exec_resources,
                                                           Buffer** buffers,
                                                           Semaphore** wait_semaphores,
                                                           Semaphore** signal_semaphores) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }

  // Executes a buffer of commands. `semaphores` is a set of semaphores that may be used by the
  // commands; the exact usage is driver-dependent.
  virtual magma_status_t ExecuteImmediateCommands(cpp20::span<uint8_t> commands,
                                                  cpp20::span<Semaphore*> semaphores) {
    return MAGMA_STATUS_UNIMPLEMENTED;
  }
};

class Buffer {
 public:
  virtual ~Buffer() = 0;
};

class Semaphore {
 public:
  virtual ~Semaphore() = 0;
};

}  // namespace msd

#endif  // SRC_GRAPHICS_LIB_MAGMA_INCLUDE_MSD_MSD_CC_H_
