// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_PLATFORM_CONNECTION_H
#define ZIRCON_PLATFORM_CONNECTION_H

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/task.h>
#include <lib/async/time.h>
#include <lib/async/wait.h>
#include <lib/stdcompat/optional.h>
#include <lib/zx/channel.h>
#include <lib/zx/profile.h>
#include <zircon/status.h>

#include "magma_util/dlog.h"
#include "msd/msd_defs.h"
#include "platform_handle.h"
#include "platform_object.h"
#include "platform_thread.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_event.h"

namespace magma {

class PlatformPerfCountPool {
 public:
  virtual ~PlatformPerfCountPool() = default;
  virtual uint64_t pool_id() = 0;
  // Sends a OnPerformanceCounterReadCompleted. May be called from any thread.
  virtual magma::Status SendPerformanceCounterCompletion(uint32_t trigger_id, uint64_t buffer_id,
                                                         uint32_t buffer_offset, uint64_t time,
                                                         uint32_t result_flags) = 0;
};

static_assert(sizeof(msd_notification_t) == 4096, "msd_notification_t is not a page");

inline void CopyNotification(const msd_notification_t* src, msd_notification_t* dst) {
  dst->type = src->type;
  switch (dst->type) {
    case MSD_CONNECTION_NOTIFICATION_CHANNEL_SEND:
      DASSERT(src->u.channel_send.size <= MSD_CHANNEL_SEND_MAX_SIZE);
      memcpy(dst->u.channel_send.data, src->u.channel_send.data, src->u.channel_send.size);
      dst->u.channel_send.size = src->u.channel_send.size;
      break;

    case MSD_CONNECTION_NOTIFICATION_PERFORMANCE_COUNTERS_READ_COMPLETED:
      memcpy(&dst->u.perf_counter_result, &src->u.perf_counter_result,
             sizeof(src->u.perf_counter_result));
      break;

    case MSD_CONNECTION_NOTIFICATION_CONTEXT_KILLED:
      break;

    case MSD_CONNECTION_NOTIFICATION_HANDLE_WAIT:
      dst->u.handle_wait = src->u.handle_wait;
      break;

    case MSD_CONNECTION_NOTIFICATION_HANDLE_WAIT_CANCEL:
      dst->u.handle_wait_cancel = src->u.handle_wait_cancel;
      break;

    default:
      DMESSAGE("Unhandled notification type: %lu", dst->type);
      DASSERT(false);
  }
}

class ZirconConnection : public fidl::WireServer<fuchsia_gpu_magma::Primary> {
 public:
  static constexpr uint32_t kMaxInflightMessages = 1000;
  static constexpr uint32_t kMaxInflightMemoryMB = 100;
  static constexpr uint32_t kMaxInflightBytes = kMaxInflightMemoryMB * 1024 * 1024;
  class Delegate {
   public:
    virtual ~Delegate() {}
    virtual magma::Status ImportObject(uint32_t handle, PlatformObject::Type object_type,
                                       uint64_t client_id) = 0;
    virtual magma::Status ReleaseObject(uint64_t object_id, PlatformObject::Type object_type) = 0;

    virtual magma::Status CreateContext(uint32_t context_id) = 0;
    virtual magma::Status DestroyContext(uint32_t context_id) = 0;

    virtual magma::Status ExecuteCommandBufferWithResources(
        uint32_t context_id, std::unique_ptr<magma_command_buffer> command_buffer,
        std::vector<magma_exec_resource> resources, std::vector<uint64_t> semaphores) = 0;
    virtual magma::Status MapBuffer(uint64_t buffer_id, uint64_t gpu_va, uint64_t offset,
                                    uint64_t length, uint64_t flags) = 0;
    virtual magma::Status UnmapBuffer(uint64_t buffer_id, uint64_t gpu_va) = 0;
    virtual magma::Status BufferRangeOp(uint64_t buffer_id, uint32_t op, uint64_t start,
                                        uint64_t length) = 0;

    virtual void SetNotificationCallback(msd_connection_notification_callback_t callback,
                                         void* token) = 0;
    virtual magma::Status ExecuteImmediateCommands(uint32_t context_id, uint64_t commands_size,
                                                   void* commands, uint64_t semaphore_count,
                                                   uint64_t* semaphore_ids) = 0;
    virtual magma::Status EnablePerformanceCounterAccess(
        std::unique_ptr<magma::PlatformHandle> access_token) = 0;
    virtual bool IsPerformanceCounterAccessAllowed() = 0;
    virtual magma::Status EnablePerformanceCounters(const uint64_t* counters,
                                                    uint64_t counter_count) = 0;
    virtual magma::Status CreatePerformanceCounterBufferPool(
        std::unique_ptr<PlatformPerfCountPool> pool) = 0;
    virtual magma::Status ReleasePerformanceCounterBufferPool(uint64_t pool_id) = 0;
    virtual magma::Status AddPerformanceCounterBufferOffsetToPool(uint64_t pool_id,
                                                                  uint64_t buffer_id,
                                                                  uint64_t buffer_offset,
                                                                  uint64_t buffer_size) = 0;
    virtual magma::Status RemovePerformanceCounterBufferFromPool(uint64_t pool_id,
                                                                 uint64_t buffer_id) = 0;
    virtual magma::Status DumpPerformanceCounters(uint64_t pool_id, uint32_t trigger_id) = 0;
    virtual magma::Status ClearPerformanceCounters(const uint64_t* counters,
                                                   uint64_t counter_count) = 0;
  };
  struct AsyncWait : public async_wait {
    AsyncWait(ZirconConnection* connection, zx_handle_t object, zx_signals_t trigger) {
      this->state = ASYNC_STATE_INIT;
      this->handler = AsyncWaitHandlerStatic;
      this->object = object;
      this->trigger = trigger;
      this->options = 0;
      this->connection = connection;
    }
    ZirconConnection* connection;
  };

  struct AsyncTask : public async_task {
    AsyncTask(ZirconConnection* connection, msd_notification_t* notification) {
      this->state = ASYNC_STATE_INIT;
      this->handler = AsyncTaskHandlerStatic;
      this->deadline = async_now(connection->async_loop()->dispatcher());
      this->connection = connection;
      CopyNotification(notification, &this->notification);
    }

    ZirconConnection* connection;
    msd_notification_t notification;
  };

  static std::shared_ptr<ZirconConnection> Create(
      std::unique_ptr<Delegate> delegate, msd_client_id_t client_id,
      std::unique_ptr<magma::PlatformHandle> server_endpoint,
      std::unique_ptr<magma::PlatformHandle> server_notification_endpoint);

  ZirconConnection(std::unique_ptr<Delegate> delegate, msd_client_id_t client_id,
                   zx::channel server_notification_endpoint,
                   std::shared_ptr<magma::PlatformEvent> shutdown_event)
      : client_id_(client_id),
        shutdown_event_(shutdown_event),
        delegate_(std::move(delegate)),
        server_notification_endpoint_(std::move(server_notification_endpoint)),
        async_loop_(&kAsyncLoopConfigNeverAttachToThread),
        async_wait_shutdown_(
            this, static_cast<magma::ZirconPlatformEvent*>(shutdown_event.get())->zx_handle(),
            ZX_EVENT_SIGNALED) {
    delegate_->SetNotificationCallback(NotificationCallbackStatic, this);
  }

  ~ZirconConnection() { delegate_->SetNotificationCallback(nullptr, 0); }

  bool Bind(zx::channel server_endpoint);

  bool HandleRequest();

  bool BeginShutdownWait();

  async::Loop* async_loop() { return &async_loop_; }

  std::shared_ptr<magma::PlatformEvent> ShutdownEvent() { return shutdown_event_; }

  static void RunLoop(std::shared_ptr<magma::ZirconConnection> connection, void* device_handle) {
    magma::PlatformThreadHelper::SetCurrentThreadName("ConnectionThread " +
                                                      std::to_string(connection->client_id_));

    // Apply the thread role before entering the handler loop.
    magma::PlatformThreadHelper::SetRole(device_handle, "fuchsia.graphics.magma.connection");

    while (connection->HandleRequest()) {
      connection->request_count_ += 1;
    }
    // the runloop terminates when the remote closes, or an error is experienced
    // so this is the appropriate time to let the connection go out of scope and be destroyed
  }

  uint64_t get_request_count() { return request_count_; }

 private:
  static void AsyncWaitHandlerStatic(async_dispatcher_t* dispatcher, async_wait_t* async_wait,
                                     zx_status_t status, const zx_packet_signal_t* signal) {
    auto wait = static_cast<AsyncWait*>(async_wait);
    wait->connection->AsyncWaitHandler(dispatcher, wait, status, signal);
  }

  void AsyncWaitHandler(async_dispatcher_t* dispatcher, AsyncWait* wait, zx_status_t status,
                        const zx_packet_signal_t* signal);

  // Could occur on an arbitrary thread (see |msd_connection_set_notification_callback|).
  // MSD must ensure we aren't in the process of destroying our connection.
  static void NotificationCallbackStatic(void* token, msd_notification_t* notification) {
    auto connection = static_cast<ZirconConnection*>(token);
    zx_status_t status = async_post_task(connection->async_loop()->dispatcher(),
                                         new AsyncTask(connection, notification));
    if (status != ZX_OK)
      DLOG("async_post_task failed, status %s", zx_status_get_string(status));
  }

  static void AsyncTaskHandlerStatic(async_dispatcher_t* dispatcher, async_task_t* async_task,
                                     zx_status_t status) {
    auto task = static_cast<AsyncTask*>(async_task);
    task->connection->AsyncTaskHandler(dispatcher, task, status);
    delete task;
  }

  bool AsyncTaskHandler(async_dispatcher_t* dispatcher, AsyncTask* task, zx_status_t status);

  void ImportObject2(ImportObject2RequestView request,
                     ImportObject2Completer::Sync& _completer) override;
  void ReleaseObject(ReleaseObjectRequestView request,
                     ReleaseObjectCompleter::Sync& _completer) override;
  void CreateContext(CreateContextRequestView request,
                     CreateContextCompleter::Sync& _completer) override;
  void DestroyContext(DestroyContextRequestView request,
                      DestroyContextCompleter::Sync& _completer) override;
  void ExecuteImmediateCommands(ExecuteImmediateCommandsRequestView request,
                                ExecuteImmediateCommandsCompleter::Sync& _completer) override;
  void ExecuteCommand(ExecuteCommandRequestView request,
                      ExecuteCommandCompleter::Sync& completer) override;
  void Flush(FlushCompleter::Sync& _completer) override;
  void MapBuffer(MapBufferRequestView request, MapBufferCompleter::Sync& _completer) override;
  void UnmapBuffer(UnmapBufferRequestView request, UnmapBufferCompleter::Sync& _completer) override;
  void BufferRangeOp2(BufferRangeOp2RequestView request,
                      BufferRangeOp2Completer::Sync& completer) override;
  void EnablePerformanceCounterAccess(
      EnablePerformanceCounterAccessRequestView request,
      EnablePerformanceCounterAccessCompleter::Sync& completer) override;
  void IsPerformanceCounterAccessAllowed(
      IsPerformanceCounterAccessAllowedCompleter::Sync& completer) override;

  void EnableFlowControl(EnableFlowControlCompleter::Sync& _completer) override;

  std::pair<uint64_t, uint64_t> GetFlowControlCounts() {
    return {messages_consumed_, bytes_imported_};
  }

  void EnablePerformanceCounters(EnablePerformanceCountersRequestView request,
                                 EnablePerformanceCountersCompleter::Sync& completer) override;
  void CreatePerformanceCounterBufferPool(
      CreatePerformanceCounterBufferPoolRequestView request,
      CreatePerformanceCounterBufferPoolCompleter::Sync& completer) override;
  void ReleasePerformanceCounterBufferPool(
      ReleasePerformanceCounterBufferPoolRequestView request,
      ReleasePerformanceCounterBufferPoolCompleter::Sync& completer) override;
  void AddPerformanceCounterBufferOffsetsToPool(
      AddPerformanceCounterBufferOffsetsToPoolRequestView request,
      AddPerformanceCounterBufferOffsetsToPoolCompleter::Sync& completer) override;
  void RemovePerformanceCounterBufferFromPool(
      RemovePerformanceCounterBufferFromPoolRequestView request,
      RemovePerformanceCounterBufferFromPoolCompleter::Sync& completer) override;
  void DumpPerformanceCounters(DumpPerformanceCountersRequestView request,
                               DumpPerformanceCountersCompleter::Sync& completer) override;
  void ClearPerformanceCounters(ClearPerformanceCountersRequestView request,
                                ClearPerformanceCountersCompleter::Sync& completer) override;

  // Epitaph will be sent on the given completer if provided, else on the server binding.
  void SetError(fidl::CompleterBase* completer, magma_status_t error);

  void FlowControl(uint64_t size = 0);

  msd_client_id_t client_id_;
  std::shared_ptr<magma::PlatformEvent> shutdown_event_;
  std::atomic_uint request_count_{};

  // The binding will be valid after a successful |fidl::BindServer| operation,
  // and back to invalid after this class is unbound from the FIDL dispatcher.
  cpp17::optional<fidl::ServerBindingRef<fuchsia_gpu_magma::Primary>> server_binding_;

  std::unique_ptr<Delegate> delegate_;
  magma_status_t error_{};
  zx::channel server_notification_endpoint_;
  zx::channel performance_counter_event_channel_;
  async::Loop async_loop_;
  AsyncWait async_wait_shutdown_;

  // Flow control
  bool flow_control_enabled_ = false;
  uint64_t messages_consumed_ = 0;
  uint64_t bytes_imported_ = 0;

  friend class FlowControlChecker;
};

}  // namespace magma

#endif  // ZIRCON_PLATFORM_CONNECTION_H
