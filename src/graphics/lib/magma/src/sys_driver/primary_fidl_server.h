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

#include "fidl/fuchsia.gpu.magma/cpp/wire_types.h"
#include "magma_util/dlog.h"
#include "magma_util/status.h"
#include "msd.h"
#include "msd/msd_defs.h"

namespace msd {

class PerfCountPoolServer {
 public:
  virtual ~PerfCountPoolServer() = default;
  virtual uint64_t pool_id() = 0;
  // Sends a OnPerformanceCounterReadCompleted. May be called from any thread.
  virtual magma::Status SendPerformanceCounterCompletion(uint32_t trigger_id, uint64_t buffer_id,
                                                         uint32_t buffer_offset, uint64_t time,
                                                         uint32_t result_flags) = 0;
};

class PrimaryFidlServer : public fidl::WireServer<fuchsia_gpu_magma::Primary>,
                          public msd::NotificationHandler {
 public:
  static constexpr uint32_t kMaxInflightMessages = 1000;
  static constexpr uint32_t kMaxInflightMemoryMB = 100;
  static constexpr uint32_t kMaxInflightBytes = kMaxInflightMemoryMB * 1024 * 1024;
  class Delegate {
   public:
    virtual ~Delegate() {}
    virtual magma::Status ImportObject(zx::handle handle, uint64_t flags,
                                       fuchsia_gpu_magma::wire::ObjectType object_type,
                                       uint64_t client_id) = 0;
    virtual magma::Status ReleaseObject(uint64_t object_id,
                                        fuchsia_gpu_magma::wire::ObjectType object_type) = 0;

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

    virtual void SetNotificationCallback(msd::NotificationHandler* handler) = 0;
    virtual magma::Status ExecuteImmediateCommands(uint32_t context_id, uint64_t commands_size,
                                                   void* commands, uint64_t semaphore_count,
                                                   uint64_t* semaphore_ids) = 0;
    virtual magma::Status EnablePerformanceCounterAccess(zx::handle access_token) = 0;
    virtual bool IsPerformanceCounterAccessAllowed() = 0;
    virtual magma::Status EnablePerformanceCounters(const uint64_t* counters,
                                                    uint64_t counter_count) = 0;
    virtual magma::Status CreatePerformanceCounterBufferPool(
        std::unique_ptr<PerfCountPoolServer> pool) = 0;
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

  static std::shared_ptr<PrimaryFidlServer> Create(
      std::unique_ptr<Delegate> delegate, msd_client_id_t client_id,
      fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary,
      fidl::ServerEnd<fuchsia_gpu_magma::Notification> notification);

  PrimaryFidlServer(std::unique_ptr<Delegate> delegate, msd_client_id_t client_id,
                    fidl::ServerEnd<fuchsia_gpu_magma::Notification> notification)
      : client_id_(client_id),
        delegate_(std::move(delegate)),
        server_notification_endpoint_(notification.TakeChannel()),
        async_loop_(&kAsyncLoopConfigNeverAttachToThread) {
    delegate_->SetNotificationCallback(this);
  }

  ~PrimaryFidlServer() override {
    delegate_->SetNotificationCallback(nullptr);
    async_loop_.Shutdown();
    delegate_.reset();
  }

  bool Bind(fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary);

  bool HandleRequest();

  void Shutdown();

  async::Loop* async_loop() { return &async_loop_; }

  static void RunLoop(std::shared_ptr<msd::PrimaryFidlServer> connection,
                      fit::function<void(const char*)> set_thread_priority) {
    pthread_setname_np(pthread_self(),
                       ("ConnectionThread " + std::to_string(connection->client_id_)).c_str());

    // Apply the thread role before entering the handler loop.
    set_thread_priority("fuchsia.graphics.magma.connection");

    while (connection->HandleRequest()) {
      connection->request_count_ += 1;
    }
    // the runloop terminates when the remote closes, or an error is experienced
    // so this is the appropriate time to let the connection go out of scope and be destroyed
  }

  uint64_t get_request_count() { return request_count_; }

  // msd::NotificationHandler implementation.
  void NotificationChannelSend(cpp20::span<uint8_t> data) override;
  void ContextKilled() override;
  void PerformanceCounterReadCompleted(const msd::PerfCounterResult& result) override;
  async_dispatcher_t* GetAsyncDispatcher() override { return async_loop_.dispatcher(); }

 private:
  void ImportObject2(ImportObject2RequestView request,
                     ImportObject2Completer::Sync& _completer) override;
  void ImportObject(ImportObjectRequestView request,
                    ImportObjectCompleter::Sync& _completer) override;
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
  std::atomic_uint request_count_{};

  // The binding will be valid after a successful |fidl::BindServer| operation,
  // and back to invalid after this class is unbound from the FIDL dispatcher.
  cpp17::optional<fidl::ServerBindingRef<fuchsia_gpu_magma::Primary>> server_binding_;

  std::unique_ptr<Delegate> delegate_;
  magma_status_t error_{};
  zx::channel server_notification_endpoint_;
  async::Loop async_loop_;

  // Flow control
  bool flow_control_enabled_ = false;
  uint64_t messages_consumed_ = 0;
  uint64_t bytes_imported_ = 0;

  friend class FlowControlChecker;
};

}  // namespace msd

#endif  // ZIRCON_PLATFORM_CONNECTION_H
