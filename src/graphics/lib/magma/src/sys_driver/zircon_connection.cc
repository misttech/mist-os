// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon_connection.h"

#include <lib/async/cpp/task.h>
#include <lib/magma/magma_common_defs.h>

#include <optional>

#include "fidl/fuchsia.gpu.magma/cpp/wire_types.h"
#include "platform_trace.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_status.h"

namespace {
std::optional<fuchsia_gpu_magma::ObjectType> ValidateObjectType(
    fuchsia_gpu_magma::ObjectType fidl_type) {
  switch (fidl_type) {
    case fuchsia_gpu_magma::ObjectType::kBuffer:
    case fuchsia_gpu_magma::ObjectType::kEvent:
      return {fidl_type};
    default:
      return std::nullopt;
  }
}

std::optional<int> GetBufferOp(fuchsia_gpu_magma::BufferOp fidl_type) {
  switch (fidl_type) {
    case fuchsia_gpu_magma::wire::BufferOp::kPopulateTables:
      return MAGMA_BUFFER_RANGE_OP_POPULATE_TABLES;
    case fuchsia_gpu_magma::wire::BufferOp::kDepopulateTables:
      return MAGMA_BUFFER_RANGE_OP_DEPOPULATE_TABLES;
    default:
      return std::nullopt;
  }
}

}  // namespace

namespace magma {

class ZirconPlatformPerfCountPool : public PlatformPerfCountPool {
 public:
  ZirconPlatformPerfCountPool(uint64_t id, zx::channel channel)
      : pool_id_(id), server_end_(std::move(channel)) {}

  uint64_t pool_id() override { return pool_id_; }

  // Sends a OnPerformanceCounterReadCompleted. May be called from any thread.
  magma::Status SendPerformanceCounterCompletion(uint32_t trigger_id, uint64_t buffer_id,
                                                 uint32_t buffer_offset, uint64_t time,
                                                 uint32_t result_flags) override {
    fidl::Arena allocator;
    auto builder = fuchsia_gpu_magma::wire::
        PerformanceCounterEventsOnPerformanceCounterReadCompletedRequest::Builder(allocator);
    builder.trigger_id(trigger_id)
        .buffer_id(buffer_id)
        .buffer_offset(buffer_offset)
        .timestamp(time)
        .flags(fuchsia_gpu_magma::wire::ResultFlags::TruncatingUnknown(result_flags));

    fidl::Status result =
        fidl::WireSendEvent(server_end_)->OnPerformanceCounterReadCompleted(builder.Build());
    switch (result.status()) {
      case ZX_OK:
        return MAGMA_STATUS_OK;
      case ZX_ERR_PEER_CLOSED:
        return MAGMA_STATUS_CONNECTION_LOST;
      case ZX_ERR_TIMED_OUT:
        return MAGMA_STATUS_TIMED_OUT;
      default:
        return MAGMA_STATUS_INTERNAL_ERROR;
    }
  }

 private:
  uint64_t pool_id_;
  fidl::ServerEnd<fuchsia_gpu_magma::PerformanceCounterEvents> server_end_;
};

void ZirconConnection::SetError(fidl::CompleterBase* completer, magma_status_t error) {
  if (!error_) {
    error_ = MAGMA_DRET_MSG(error, "ZirconConnection encountered dispatcher error");
    if (completer) {
      completer->Close(magma::ToZxStatus(error));
    } else {
      server_binding_->Close(magma::ToZxStatus(error));
    }
    async_loop()->Quit();
  }
}

bool ZirconConnection::Bind(fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary) {
  fidl::OnUnboundFn<ZirconConnection> unbind_callback =
      [](ZirconConnection* self, fidl::UnbindInfo unbind_info,
         fidl::ServerEnd<fuchsia_gpu_magma::Primary> server_channel) {
        // |kDispatcherError| indicates the async loop itself is shutting down,
        // which could only happen when |interface| is being destructed.
        // Therefore, we must avoid using the same object.
        if (unbind_info.reason() == fidl::Reason::kDispatcherError)
          return;

        self->server_binding_ = cpp17::nullopt;
        self->async_loop()->Quit();
      };

  // Note: the async loop should not be started until we assign |server_binding_|.
  server_binding_ = fidl::BindServer(async_loop()->dispatcher(), std::move(primary), this,
                                     std::move(unbind_callback));
  return true;
}

void ZirconConnection::Shutdown() {
  async::PostTask(async_loop()->dispatcher(), [this]() {
    if (server_binding_) {
      server_binding_->Close(ZX_ERR_CANCELED);
    }
    async_loop()->Quit();
  });
}

bool ZirconConnection::HandleRequest() {
  zx_status_t status = async_loop_.Run(zx::time::infinite(), true /* once */);
  if (status != ZX_OK)
    return false;
  return true;
}

void ZirconConnection::NotificationChannelSend(cpp20::span<uint8_t> data) {
  zx_status_t status = server_notification_endpoint_.write(
      0, data.data(), static_cast<uint32_t>(data.size()), nullptr, 0);
  if (status != ZX_OK)
    MAGMA_DLOG("Failed writing to channel: %s", zx_status_get_string(status));
}
void ZirconConnection::ContextKilled() {
  async::PostTask(async_loop_.dispatcher(),
                  [this]() { SetError(nullptr, MAGMA_STATUS_CONTEXT_KILLED); });
}

void ZirconConnection::PerformanceCounterReadCompleted(const msd::PerfCounterResult& result) {
  MAGMA_DASSERT(false);
}

void ZirconConnection::EnableFlowControl(EnableFlowControlCompleter::Sync& completer) {
  flow_control_enabled_ = true;
}

void ZirconConnection::FlowControl(uint64_t size) {
  if (!flow_control_enabled_)
    return;

  messages_consumed_ += 1;
  bytes_imported_ += size;

  if (messages_consumed_ >= kMaxInflightMessages / 2) {
    fidl::Status result =
        fidl::WireSendEvent(server_binding_.value())->OnNotifyMessagesConsumed(messages_consumed_);
    if (result.ok()) {
      messages_consumed_ = 0;
    } else if (!result.is_canceled() && !result.is_peer_closed()) {
      MAGMA_DMESSAGE("SendOnNotifyMessagesConsumedEvent failed: %s",
                     result.FormatDescription().c_str());
    }
  }

  if (bytes_imported_ >= kMaxInflightBytes / 2) {
    fidl::Status result =
        fidl::WireSendEvent(server_binding_.value())->OnNotifyMemoryImported(bytes_imported_);
    if (result.ok()) {
      bytes_imported_ = 0;
    } else if (!result.is_canceled() && !result.is_peer_closed()) {
      MAGMA_DMESSAGE("SendOnNotifyMemoryImportedEvent failed: %s",
                     result.FormatDescription().c_str());
    }
  }
}

void ZirconConnection::ImportObject2(ImportObject2RequestView request,
                                     ImportObject2Completer::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::ImportObject2", "type",
                 static_cast<uint32_t>(request->object_type));
  MAGMA_DLOG("ZirconConnection: ImportObject2");

  auto object_type = ValidateObjectType(request->object_type);
  if (!object_type) {
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
    return;
  }

  uint64_t size = 0;

  if (object_type == fuchsia_gpu_magma::wire::ObjectType::kBuffer) {
    zx::unowned_vmo vmo(request->object.get());
    zx_status_t status = vmo->get_size(&size);
    if (status != ZX_OK) {
      SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
      return;
    }
  }
  FlowControl(size);

  if (!delegate_->ImportObject(std::move(request->object), *object_type, request->object_id))
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
}

void ZirconConnection::ReleaseObject(ReleaseObjectRequestView request,
                                     ReleaseObjectCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::ReleaseObject", "type",
                 static_cast<uint32_t>(request->object_type));
  MAGMA_DLOG("ZirconConnection: ReleaseObject");
  FlowControl();

  auto object_type = ValidateObjectType(request->object_type);
  if (!object_type) {
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
    return;
  }

  if (!delegate_->ReleaseObject(request->object_id, *object_type))
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
}

void ZirconConnection::CreateContext(CreateContextRequestView request,
                                     CreateContextCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::CreateContext");
  MAGMA_DLOG("ZirconConnection: CreateContext");
  FlowControl();

  magma::Status status = delegate_->CreateContext(request->context_id);
  if (!status.ok())
    SetError(&completer, status.get());
}

void ZirconConnection::DestroyContext(DestroyContextRequestView request,
                                      DestroyContextCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::DestroyContext");
  MAGMA_DLOG("ZirconConnection: DestroyContext");
  FlowControl();

  magma::Status status = delegate_->DestroyContext(request->context_id);
  if (!status.ok())
    SetError(&completer, status.get());
}

void ZirconConnection::ExecuteCommand(ExecuteCommandRequestView request,
                                      ExecuteCommandCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::ExecuteCommand");
  FlowControl();

  // TODO(fxbug.dev/92606) - support > 1 command buffer
  if (request->command_buffers.count() > 1) {
    SetError(&completer, MAGMA_STATUS_UNIMPLEMENTED);
    return;
  }

  auto command_buffer = std::make_unique<magma_command_buffer>();

  *command_buffer = {
      .resource_count = static_cast<uint32_t>(request->resources.count()),
      .batch_buffer_resource_index = request->command_buffers[0].resource_index,
      .batch_start_offset = request->command_buffers[0].start_offset,
      .wait_semaphore_count = static_cast<uint32_t>(request->wait_semaphores.count()),
      .signal_semaphore_count = static_cast<uint32_t>(request->signal_semaphores.count()),
      .flags = static_cast<uint64_t>(request->flags),
  };

  std::vector<magma_exec_resource> resources;
  resources.reserve(request->resources.count());

  for (auto& buffer_range : request->resources) {
    resources.push_back({
        buffer_range.buffer_id,
        buffer_range.offset,
        buffer_range.size,
    });
  }

  // Merge semaphores into one vector
  std::vector<uint64_t> semaphores;
  semaphores.reserve(request->wait_semaphores.count() + request->signal_semaphores.count());

  for (uint64_t semaphore_id : request->wait_semaphores) {
    semaphores.push_back(semaphore_id);
  }
  for (uint64_t semaphore_id : request->signal_semaphores) {
    semaphores.push_back(semaphore_id);
  }

  magma::Status status = delegate_->ExecuteCommandBufferWithResources(
      request->context_id, std::move(command_buffer), std::move(resources), std::move(semaphores));

  if (!status)
    SetError(&completer, status.get());
}

void ZirconConnection::ExecuteImmediateCommands(
    ExecuteImmediateCommandsRequestView request,
    ExecuteImmediateCommandsCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::ExecuteImmediateCommands");
  MAGMA_DLOG("ZirconConnection: ExecuteImmediateCommands");
  FlowControl();

  magma::Status status = delegate_->ExecuteImmediateCommands(
      request->context_id, request->command_data.count(), request->command_data.data(),
      request->semaphores.count(), request->semaphores.data());
  if (!status)
    SetError(&completer, status.get());
}

void ZirconConnection::Flush(FlushCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::Flush");
  MAGMA_DLOG("ZirconConnection: Flush");
  completer.Reply();
}

void ZirconConnection::MapBuffer(MapBufferRequestView request,
                                 MapBufferCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::MapBuffer");
  MAGMA_DLOG("ZirconConnection: MapBufferFIDL");
  FlowControl();

  if (!request->has_range() || !request->has_hw_va()) {
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
    return;
  }

  auto flags = request->has_flags() ? static_cast<uint64_t>(request->flags()) : 0;

  magma::Status status =
      delegate_->MapBuffer(request->range().buffer_id, request->hw_va(), request->range().offset,
                           request->range().size, flags);
  if (!status.ok())
    SetError(&completer, status.get());
}

void ZirconConnection::UnmapBuffer(UnmapBufferRequestView request,
                                   UnmapBufferCompleter::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::UnmapBuffer");
  MAGMA_DLOG("ZirconConnection: UnmapBufferFIDL");
  FlowControl();

  if (!request->has_buffer_id() || !request->has_hw_va()) {
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
    return;
  }

  magma::Status status = delegate_->UnmapBuffer(request->buffer_id(), request->hw_va());
  if (!status.ok())
    SetError(&completer, status.get());
}

void ZirconConnection::BufferRangeOp2(BufferRangeOp2RequestView request,
                                      BufferRangeOp2Completer::Sync& completer) {
  TRACE_DURATION("magma", "ZirconConnection::BufferRangeOp2");
  MAGMA_DLOG("ZirconConnection:::BufferRangeOp2");
  FlowControl();

  std::optional<int> buffer_op = GetBufferOp(request->op);
  if (!buffer_op) {
    SetError(&completer, MAGMA_STATUS_INVALID_ARGS);
    return;
  }

  magma::Status status = delegate_->BufferRangeOp(request->range.buffer_id, *buffer_op,
                                                  request->range.offset, request->range.size);

  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::EnablePerformanceCounterAccess(
    EnablePerformanceCounterAccessRequestView request,
    EnablePerformanceCounterAccessCompleter::Sync& completer) {
  MAGMA_DLOG("ZirconConnection:::EnablePerformanceCounterAccess");
  FlowControl();

  magma::Status status =
      delegate_->EnablePerformanceCounterAccess(std::move(request->access_token));
  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::IsPerformanceCounterAccessAllowed(
    IsPerformanceCounterAccessAllowedCompleter::Sync& completer) {
  MAGMA_DLOG("ZirconConnection:::IsPerformanceCounterAccessAllowed");
  completer.Reply(delegate_->IsPerformanceCounterAccessAllowed());
}

void ZirconConnection::EnablePerformanceCounters(
    EnablePerformanceCountersRequestView request,
    EnablePerformanceCountersCompleter::Sync& completer) {
  FlowControl();
  magma::Status status =
      delegate_->EnablePerformanceCounters(request->counters.data(), request->counters.count());
  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::CreatePerformanceCounterBufferPool(
    CreatePerformanceCounterBufferPoolRequestView request,
    CreatePerformanceCounterBufferPoolCompleter::Sync& completer) {
  FlowControl();
  auto pool = std::make_unique<ZirconPlatformPerfCountPool>(request->pool_id,
                                                            request->event_channel.TakeChannel());
  magma::Status status = delegate_->CreatePerformanceCounterBufferPool(std::move(pool));
  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::ReleasePerformanceCounterBufferPool(
    ReleasePerformanceCounterBufferPoolRequestView request,
    ReleasePerformanceCounterBufferPoolCompleter::Sync& completer) {
  FlowControl();
  magma::Status status = delegate_->ReleasePerformanceCounterBufferPool(request->pool_id);
  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::AddPerformanceCounterBufferOffsetsToPool(
    AddPerformanceCounterBufferOffsetsToPoolRequestView request,
    AddPerformanceCounterBufferOffsetsToPoolCompleter::Sync& completer) {
  FlowControl();
  for (auto& offset : request->offsets) {
    magma::Status status = delegate_->AddPerformanceCounterBufferOffsetToPool(
        request->pool_id, offset.buffer_id, offset.offset, offset.size);
    if (!status) {
      SetError(&completer, status.get());
    }
  }
}

void ZirconConnection::RemovePerformanceCounterBufferFromPool(
    RemovePerformanceCounterBufferFromPoolRequestView request,
    RemovePerformanceCounterBufferFromPoolCompleter::Sync& completer) {
  FlowControl();
  magma::Status status =
      delegate_->RemovePerformanceCounterBufferFromPool(request->pool_id, request->buffer_id);
  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::DumpPerformanceCounters(DumpPerformanceCountersRequestView request,
                                               DumpPerformanceCountersCompleter::Sync& completer) {
  FlowControl();
  magma::Status status = delegate_->DumpPerformanceCounters(request->pool_id, request->trigger_id);
  if (!status) {
    SetError(&completer, status.get());
  }
}

void ZirconConnection::ClearPerformanceCounters(
    ClearPerformanceCountersRequestView request,
    ClearPerformanceCountersCompleter::Sync& completer) {
  FlowControl();
  magma::Status status =
      delegate_->ClearPerformanceCounters(request->counters.data(), request->counters.count());
  if (!status) {
    SetError(&completer, status.get());
  }
}

// static
std::shared_ptr<ZirconConnection> ZirconConnection::Create(
    std::unique_ptr<Delegate> delegate, msd_client_id_t client_id,
    fidl::ServerEnd<fuchsia_gpu_magma::Primary> primary,
    fidl::ServerEnd<fuchsia_gpu_magma::Notification> notification) {
  if (!delegate)
    return MAGMA_DRETP(nullptr, "attempting to create PlatformConnection with null delegate");

  auto connection =
      std::make_shared<ZirconConnection>(std::move(delegate), client_id, std::move(notification));

  if (!connection->Bind(std::move(primary)))
    return MAGMA_DRETP(nullptr, "Bind failed");

  return connection;
}

}  // namespace magma
