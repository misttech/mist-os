// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/tracee.h"

#include <lib/async/default.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-engine/fields.h>
#include <lib/trace-provider/provider.h>

#include <memory>

#include <fbl/algorithm.h>

#include "lib/fit/defer.h"
#include "src/performance/trace_manager/trace_session.h"
#include "src/performance/trace_manager/util.h"

namespace tracing {

namespace {

fuchsia::tracing::BufferingMode EngineBufferingModeToProviderMode(trace_buffering_mode_t mode) {
  switch (mode) {
    case TRACE_BUFFERING_MODE_ONESHOT:
      return fuchsia::tracing::BufferingMode::ONESHOT;
    case TRACE_BUFFERING_MODE_CIRCULAR:
      return fuchsia::tracing::BufferingMode::CIRCULAR;
    case TRACE_BUFFERING_MODE_STREAMING:
      return fuchsia::tracing::BufferingMode::STREAMING;
    default:
      __UNREACHABLE;
  }
}
}  // namespace

Tracee::Tracee(async::Executor& executor, std::shared_ptr<const BufferForwarder> output,
               const TraceProviderBundle* bundle)
    : output_(std::move(output)),
      bundle_(bundle),
      executor_(executor),
      wait_(this),
      weak_ptr_factory_(this) {}

bool Tracee::operator==(TraceProviderBundle* bundle) const { return bundle_ == bundle; }

bool Tracee::Initialize(fidl::VectorPtr<std::string> categories, size_t buffer_size,
                        fuchsia::tracing::BufferingMode buffering_mode,
                        StartCallback start_callback, StopCallback stop_callback,
                        TerminateCallback terminate_callback, AlertCallback alert_callback) {
  FX_DCHECK(state_ == State::kReady);
  FX_DCHECK(!buffer_vmo_);
  FX_DCHECK(start_callback);
  FX_DCHECK(stop_callback);
  FX_DCHECK(terminate_callback);
  FX_DCHECK(alert_callback);

  // HACK(https://fxbug.dev/308796439): Until we get kernel trace streameing, kernel tracing is
  // special: it always allocates a fixed sized buffer in the kernel set by a boot arg. We're not at
  // liberty here in trace_manager to check what the bootarg is, but the default is 32MB. For
  // ktrace_provider, we should allocate a buffer at least large enough to hold the full kernel
  // trace.
  if (bundle_->name == "ktrace_provider") {
    buffer_size = std::max(buffer_size, size_t{32} * 1024 * 1024);
  }

  zx::vmo buffer_vmo;
  zx_status_t status = zx::vmo::create(buffer_size, 0u, &buffer_vmo);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << *bundle_ << ": Failed to create trace buffer: status=" << status;
    return false;
  }

  zx::vmo buffer_vmo_for_provider;
  status =
      buffer_vmo.duplicate(ZX_RIGHTS_BASIC | ZX_RIGHTS_IO | ZX_RIGHT_MAP, &buffer_vmo_for_provider);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << *bundle_
                   << ": Failed to duplicate trace buffer for provider: status=" << status;
    return false;
  }

  zx::fifo fifo, fifo_for_provider;
  status = zx::fifo::create(kFifoSizeInPackets, sizeof(trace_provider_packet_t), 0u, &fifo,
                            &fifo_for_provider);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << *bundle_ << ": Failed to create trace buffer fifo: status=" << status;
    return false;
  }

  fuchsia::tracing::provider::ProviderConfig provider_config;
  provider_config.buffering_mode = buffering_mode;
  provider_config.buffer = std::move(buffer_vmo_for_provider);
  provider_config.fifo = std::move(fifo_for_provider);
  if (categories.has_value()) {
    provider_config.categories = std::move(categories.value());
  }
  bundle_->provider->Initialize(std::move(provider_config));

  buffering_mode_ = buffering_mode;
  buffer_vmo_ = std::move(buffer_vmo);
  buffer_vmo_size_ = buffer_size;
  fifo_ = std::move(fifo);

  start_callback_ = std::move(start_callback);
  stop_callback_ = std::move(stop_callback);
  terminate_callback_ = std::move(terminate_callback);
  alert_callback_ = std::move(alert_callback);

  wait_.set_object(fifo_.get());
  wait_.set_trigger(ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED);
  status = wait_.Begin(executor_.dispatcher());
  FX_CHECK(status == ZX_OK) << "Failed to add handler: status=" << status;

  TransitionToState(State::kInitialized);
  return true;
}

void Tracee::Terminate() {
  if (state_ == State::kTerminating || state_ == State::kTerminated) {
    return;
  }
  bundle_->provider->Terminate();
  TransitionToState(State::kTerminating);
}

void Tracee::Start(fuchsia::tracing::BufferDisposition buffer_disposition,
                   const std::vector<std::string>& additional_categories) {
  // TraceSession should not call us unless we're ready, either because this
  // is the first time, or subsequent times after tracing has fully stopped
  // from the preceding time.
  FX_DCHECK(state_ == State::kInitialized || state_ == State::kStopped);

  fuchsia::tracing::provider::StartOptions start_options;
  start_options.buffer_disposition = buffer_disposition;
  start_options.additional_categories = additional_categories;
  bundle_->provider->Start(std::move(start_options));

  TransitionToState(State::kStarting);
  was_started_ = true;
  results_written_ = false;
}

void Tracee::Stop(bool write_results) {
  if (state_ != State::kStarting && state_ != State::kStarted) {
    if (state_ == State::kInitialized) {
      // We must have gotten added after tracing started while tracing was
      // being stopped. Mark us as stopped so TraceSession won't try to wait
      // for us to do so.
      TransitionToState(State::kStopped);
    }
    return;
  }
  bundle_->provider->Stop();
  TransitionToState(State::kStopping);
  write_results_ = write_results;
}

void Tracee::TransitionToState(State new_state) {
  FX_LOGS(DEBUG) << *bundle_ << ": Transitioning from " << state_ << " to " << new_state;
  state_ = new_state;
}

void Tracee::OnHandleReady(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                           zx_status_t status, const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    OnHandleError(status);
    return;
  }

  zx_signals_t pending = signal->observed;
  FX_LOGS(DEBUG) << *bundle_ << ": pending=0x" << std::hex << pending;
  FX_DCHECK(pending & (ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED));
  FX_DCHECK(state_ != State::kReady && state_ != State::kTerminated);

  if (pending & ZX_FIFO_READABLE) {
    OnFifoReadable(dispatcher, wait);
    // Keep reading packets, one per call, until the peer goes away.
    status = wait->Begin(dispatcher);
    if (status != ZX_OK)
      OnHandleError(status);
    return;
  }

  FX_DCHECK(pending & ZX_FIFO_PEER_CLOSED);
  wait_.set_object(ZX_HANDLE_INVALID);
  TransitionToState(State::kTerminated);
  fit::closure terminate_callback = std::move(terminate_callback_);
  FX_DCHECK(terminate_callback);
  terminate_callback();
}

void Tracee::OnFifoReadable(async_dispatcher_t* dispatcher, async::WaitBase* wait) {
  trace_provider_packet_t packet;
  auto status2 = zx_fifo_read(wait_.object(), sizeof(packet), &packet, 1u, nullptr);
  FX_DCHECK(status2 == ZX_OK);
  if (packet.data16 != 0 && packet.request != TRACE_PROVIDER_ALERT) {
    FX_LOGS(ERROR) << *bundle_ << ": Received bad packet, non-zero data16 field: " << packet.data16;
    Abort();
    return;
  }

  switch (packet.request) {
    case TRACE_PROVIDER_STARTED:
      // The provider should only be signalling us when it has finished
      // startup.
      if (packet.data32 != TRACE_PROVIDER_FIFO_PROTOCOL_VERSION) {
        FX_LOGS(ERROR) << *bundle_
                       << ": Received bad packet, unexpected version: " << packet.data32;
        Abort();
        break;
      }
      if (packet.data64 != 0) {
        FX_LOGS(ERROR) << *bundle_
                       << ": Received bad packet, non-zero data64 field: " << packet.data64;
        Abort();
        break;
      }
      if (state_ == State::kStarting) {
        TransitionToState(State::kStarted);
        start_callback_();
      } else {
        // This could be a problem in the provider or it could just be slow.
        // TODO(dje): Disconnect it and force it to reconnect?
        FX_LOGS(WARNING) << *bundle_ << ": Received TRACE_PROVIDER_STARTED in state " << state_;
      }
      break;
    case TRACE_PROVIDER_SAVE_BUFFER:
      if (buffering_mode_ != fuchsia::tracing::BufferingMode::STREAMING) {
        FX_LOGS(WARNING) << *bundle_ << ": Received TRACE_PROVIDER_SAVE_BUFFER in mode "
                         << ModeName(buffering_mode_);
      } else if (state_ == State::kStarted || state_ == State::kStopping ||
                 state_ == State::kTerminating) {
        uint32_t wrapped_count = packet.data32;
        uint64_t durable_data_end = packet.data64;
        // Schedule the write with the main async loop.
        FX_LOGS(DEBUG) << "Buffer save request from " << *bundle_
                       << ", wrapped_count=" << wrapped_count << ", durable_data_end=0x" << std::hex
                       << durable_data_end;
        async::PostTask(executor_.dispatcher(),
                        [weak = weak_ptr_factory_.GetWeakPtr(), wrapped_count, durable_data_end] {
                          if (weak) {
                            weak->TransferBuffer(wrapped_count, durable_data_end);
                          }
                        });
      } else {
        FX_LOGS(WARNING) << *bundle_ << ": Received TRACE_PROVIDER_SAVE_BUFFER in state " << state_;
      }
      break;
    case TRACE_PROVIDER_STOPPED:
      if (packet.data16 != 0 || packet.data32 != 0 || packet.data64 != 0) {
        FX_LOGS(ERROR) << *bundle_ << ": Received bad packet, non-zero data fields";
        Abort();
        break;
      }
      if (state_ == State::kStopping || state_ == State::kTerminating) {
        // If we're terminating leave the transition to kTerminated to
        // noticing the fifo peer closed.
        if (state_ == State::kStopping) {
          TransitionToState(State::kStopped);
        }
        stop_callback_(write_results_);
      } else {
        // This could be a problem in the provider or it could just be slow.
        // TODO(dje): Disconnect it and force it to reconnect?
        FX_LOGS(WARNING) << *bundle_ << ": Received TRACE_PROVIDER_STOPPED in state " << state_;
      }
      break;
    case TRACE_PROVIDER_ALERT: {
      auto p = reinterpret_cast<const char*>(&packet.data16);
      size_t size = sizeof(packet.data16) + sizeof(packet.data32) + sizeof(packet.data64);
      std::string alert_name;
      alert_name.reserve(size);

      for (size_t i = 0; i < size && *p != 0; ++i) {
        alert_name.push_back(*p++);
      }

      alert_callback_(std::move(alert_name));
    } break;
    default:
      FX_LOGS(ERROR) << *bundle_ << ": Received bad packet, unknown request: " << packet.request;
      Abort();
      break;
  }
}

void Tracee::OnHandleError(zx_status_t status) {
  FX_LOGS(DEBUG) << *bundle_ << ": error=" << status;
  FX_DCHECK(status == ZX_ERR_CANCELED);
  FX_DCHECK(state_ != State::kReady && state_ != State::kTerminated);
  wait_.set_object(ZX_HANDLE_INVALID);
  TransitionToState(State::kTerminated);
}

bool Tracee::VerifyBufferHeader(const trace::internal::BufferHeaderReader* header) const {
  if (EngineBufferingModeToProviderMode(
          static_cast<trace_buffering_mode_t>(header->buffering_mode())) != buffering_mode_) {
    FX_LOGS(ERROR) << *bundle_
                   << ": header corrupt, wrong buffering mode: " << header->buffering_mode();
    return false;
  }

  return true;
}

TransferStatus Tracee::WriteChunk(uint64_t offset, uint64_t last, uint64_t end,
                                  uint64_t buffer_size) const {
  ZX_DEBUG_ASSERT(last <= buffer_size);
  ZX_DEBUG_ASSERT(end <= buffer_size);
  ZX_DEBUG_ASSERT(end == 0 || last <= end);
  offset += last;
  if (buffering_mode_ == fuchsia::tracing::BufferingMode::ONESHOT ||
      // If end is zero then the header wasn't updated when tracing stopped.
      end == 0) {
    uint64_t size = buffer_size - last;
    return output_->WriteChunkBy(BufferForwarder::ForwardStrategy::Records, buffer_vmo_, offset,
                                 size);
  }
  uint64_t size = end - last;
  return output_->WriteChunkBy(BufferForwarder::ForwardStrategy::Size, buffer_vmo_, offset, size);
}

TransferStatus Tracee::TransferRecords() const {
  FX_DCHECK(buffer_vmo_);

  // Regardless of whether we succeed or fail, mark results as being written.
  results_written_ = true;

  if (auto transfer_status = WriteProviderIdRecord();
      transfer_status != TransferStatus::kComplete) {
    FX_LOGS(ERROR) << *bundle_ << ": Failed to write provider info record to trace.";
    return transfer_status;
  }

  trace::internal::trace_buffer_header header_buffer;
  if (buffer_vmo_.read(&header_buffer, 0, sizeof(header_buffer)) != ZX_OK) {
    FX_LOGS(ERROR) << *bundle_ << ": Failed to read header from buffer_vmo";
    return TransferStatus::kProviderError;
  }

  std::unique_ptr<trace::internal::BufferHeaderReader> header;
  auto error =
      trace::internal::BufferHeaderReader::Create(&header_buffer, buffer_vmo_size_, &header);
  if (error != "") {
    FX_LOGS(ERROR) << *bundle_ << ": header corrupt, " << error.c_str();
    return TransferStatus::kProviderError;
  }
  if (!VerifyBufferHeader(header.get())) {
    return TransferStatus::kProviderError;
  }

  if (header->num_records_dropped() > 0) {
    FX_LOGS(WARNING) << *bundle_ << ": " << header->num_records_dropped()
                     << " records were dropped";
    // If we can't write the buffer overflow record, it's not the end of the
    // world.
    if (output_->WriteProviderBufferOverflowEvent(bundle_->id) != TransferStatus::kComplete) {
      FX_LOGS(DEBUG) << *bundle_
                     << ": Failed to write provider event (buffer overflow) record to trace.";
    }
  }

  if (buffering_mode_ != fuchsia::tracing::BufferingMode::ONESHOT) {
    uint64_t offset = header->get_durable_buffer_offset();
    uint64_t last = last_durable_data_end_;
    uint64_t end = header->durable_data_end();
    uint64_t buffer_size = header->durable_buffer_size();
    FX_LOGS(DEBUG) << "Writing durable buffer for " << bundle_->name;
    if (auto transfer_status = WriteChunk(offset, last, end, buffer_size);
        transfer_status != TransferStatus::kComplete) {
      return transfer_status;
    }
  }

  // There's only two buffers, thus the earlier one is not the current one.
  // It's important to process them in chronological order on the off
  // chance that the earlier buffer provides a stringref or threadref
  // referenced by the later buffer.
  //
  // We want to handle the case of still capturing whatever records we can if
  // the process crashes, in which case the header won't be up to date. In
  // oneshot mode we're covered: We run through the records and see what's
  // there. In circular and streaming modes after a buffer gets reused we can't
  // do that. But if the process crashes it may be the last trace records that
  // are important: we don't want to lose them. As a compromise, if the header
  // is marked as valid use it. Otherwise run through the buffer to count the
  // records we see.

  auto write_rolling_chunk = [this, &header](int buffer_number) -> TransferStatus {
    uint64_t offset = header->GetRollingBufferOffset(buffer_number);
    uint64_t last = 0;
    uint64_t end = header->rolling_data_end(buffer_number);
    uint64_t buffer_size = header->rolling_buffer_size();
    auto name = buffer_number == 0 ? "rolling buffer 0" : "rolling buffer 1";
    FX_LOGS(DEBUG) << "Writing chunks for " << name;
    return WriteChunk(offset, last, end, buffer_size);
  };

  if (header->wrapped_count() > 0) {
    int buffer_number = get_buffer_number(header->wrapped_count() - 1);
    if (auto transfer_status = write_rolling_chunk(buffer_number);
        transfer_status != TransferStatus::kComplete) {
      return transfer_status;
    }
  }
  int buffer_number = get_buffer_number(header->wrapped_count());
  if (auto transfer_status = write_rolling_chunk(buffer_number);
      transfer_status != TransferStatus::kComplete) {
    return transfer_status;
  }

  provider_stats_.set_name(bundle_->name);
  provider_stats_.set_pid(bundle_->pid);
  provider_stats_.set_buffering_mode(EngineBufferingModeToProviderMode(header->buffering_mode()));
  provider_stats_.set_buffer_wrapped_count(header->wrapped_count());
  provider_stats_.set_records_dropped(header->num_records_dropped());
  float durable_buffer_used = 0;
  if (header->durable_buffer_size() > 0) {
    durable_buffer_used = (static_cast<float>(header->durable_data_end()) /
                           static_cast<float>(header->durable_buffer_size())) *
                          100;
  }
  provider_stats_.set_percentage_durable_buffer_used(durable_buffer_used);
  provider_stats_.set_non_durable_bytes_written(header->rolling_data_end(0) +
                                                header->rolling_data_end(1));

  // Print some stats to assist things like buffer size calculations.
  // Don't print anything if nothing was written.
  // TODO(dje): Revisit this once stats are fully reported back to the client.
  if ((header->buffering_mode() == TRACE_BUFFERING_MODE_ONESHOT &&
       header->rolling_data_end(0) > kInitRecordSizeBytes) ||
      ((header->buffering_mode() != TRACE_BUFFERING_MODE_ONESHOT) &&
       header->durable_data_end() > kInitRecordSizeBytes) ||
      header->buffering_mode() == TRACE_BUFFERING_MODE_STREAMING) {
    FX_LOGS(INFO) << *bundle_ << " trace stats";
    FX_LOGS(INFO) << "Wrapped count: " << header->wrapped_count();
    FX_LOGS(INFO) << "# records dropped: " << header->num_records_dropped();
    FX_LOGS(INFO) << "Durable buffer: 0x" << std::hex << header->durable_data_end() << ", size 0x"
                  << std::hex << header->durable_buffer_size();
    FX_LOGS(INFO) << "Non-durable buffer: 0x" << std::hex << header->rolling_data_end(0) << ",0x"
                  << std::hex << header->rolling_data_end(1) << ", size 0x" << std::hex
                  << header->rolling_buffer_size();
  }

  return TransferStatus::kComplete;
}

std::optional<controller::ProviderStats> Tracee::GetStats() const {
  if (state_ == State::kTerminated || state_ == State::kStopped) {
    return std::move(provider_stats_);
  }
  return std::nullopt;
}

void Tracee::TransferBuffer(uint32_t wrapped_count, uint64_t durable_data_end) {
  FX_DCHECK(buffering_mode_ == fuchsia::tracing::BufferingMode::STREAMING);
  FX_DCHECK(buffer_vmo_);

  if (!DoTransferBuffer(wrapped_count, durable_data_end)) {
    Abort();
    return;
  }

  // If a consumer isn't connected we still want to mark the buffer as having
  // been saved in order to keep the trace engine running.
  last_wrapped_count_ = wrapped_count;
  last_durable_data_end_ = durable_data_end;
  NotifyBufferSaved(wrapped_count, durable_data_end);
}

bool Tracee::DoTransferBuffer(uint32_t wrapped_count, uint64_t durable_data_end) {
  if (wrapped_count == 0 && last_wrapped_count_ == 0) {
    // ok
  } else if (wrapped_count != last_wrapped_count_ + 1) {
    FX_LOGS(ERROR) << *bundle_ << ": unexpected wrapped_count from provider: " << wrapped_count;
    return false;
  } else if (durable_data_end < last_durable_data_end_ || (durable_data_end & 7) != 0) {
    FX_LOGS(ERROR) << *bundle_
                   << ": unexpected durable_data_end from provider: " << durable_data_end;
    return false;
  }

  int buffer_number = get_buffer_number(wrapped_count);

  if (WriteProviderIdRecord() != TransferStatus::kComplete) {
    FX_LOGS(ERROR) << *bundle_ << ": Failed to write provider section record to trace.";
    return false;
  }

  trace::internal::trace_buffer_header header_buffer;
  if (buffer_vmo_.read(&header_buffer, 0, sizeof(header_buffer)) != ZX_OK) {
    FX_LOGS(ERROR) << *bundle_ << ": Failed to read header from buffer_vmo";
    return false;
  }

  std::unique_ptr<trace::internal::BufferHeaderReader> header;
  auto error =
      trace::internal::BufferHeaderReader::Create(&header_buffer, buffer_vmo_size_, &header);
  if (error != "") {
    FX_LOGS(ERROR) << *bundle_ << ": header corrupt, " << error.c_str();
    return false;
  }
  if (!VerifyBufferHeader(header.get())) {
    return false;
  }

  FX_LOGS(DEBUG) << "Dropped records: " << header->num_records_dropped();

  // Don't use |header.durable_data_end| here, we want the value at the time
  // the message was sent.
  if (durable_data_end < kInitRecordSizeBytes || durable_data_end > header->durable_buffer_size() ||
      (durable_data_end & 7) != 0 || durable_data_end < last_durable_data_end_) {
    FX_LOGS(ERROR) << *bundle_ << ": bad durable_data_end: " << durable_data_end;
    return false;
  }

  // However we can use rolling_data_end from the header.
  // This buffer is no longer being written to until we save it.
  // [And if it does get written to it'll potentially result in corrupt
  // data, but that's not our problem; as long as we can't crash, which is
  // always the rule here.]
  uint64_t rolling_data_end = header->rolling_data_end(buffer_number);

  // Only transfer what's new in the durable buffer since the last time.
  uint64_t durable_buffer_offset = header->get_durable_buffer_offset();
  if (durable_data_end > last_durable_data_end_) {
    uint64_t size = durable_data_end - last_durable_data_end_;
    FX_LOGS(DEBUG) << "Writing durable buffer for " << bundle_->name;
    if (output_->WriteChunkBy(BufferForwarder::ForwardStrategy::Size, buffer_vmo_,
                              durable_buffer_offset + last_durable_data_end_,
                              size) != TransferStatus::kComplete) {
      return false;
    }
  }

  uint64_t buffer_offset = header->GetRollingBufferOffset(buffer_number);
  auto name = buffer_number == 0 ? "rolling buffer 0" : "rolling buffer 1";
  FX_LOGS(DEBUG) << "Writing " << name << "for " << bundle_->name;
  return output_->WriteChunkBy(BufferForwarder::ForwardStrategy::Size, buffer_vmo_, buffer_offset,
                               rolling_data_end) == TransferStatus::kComplete;
}

void Tracee::NotifyBufferSaved(uint32_t wrapped_count, uint64_t durable_data_end) {
  FX_LOGS(DEBUG) << "Buffer saved for " << *bundle_ << ", wrapped_count=" << wrapped_count
                 << ", durable_data_end=" << durable_data_end;
  trace_provider_packet_t packet{
      .request = TRACE_PROVIDER_BUFFER_SAVED,
      .data32 = wrapped_count,
      .data64 = durable_data_end,
  };
  auto status = fifo_.write(sizeof(packet), &packet, 1, nullptr);
  if (status == ZX_ERR_SHOULD_WAIT) {
    // The FIFO should never fill. If it does then the provider is sending us
    // buffer full notifications but not reading our replies. Terminate the
    // connection.
    Abort();
  } else {
    FX_DCHECK(status == ZX_OK || status == ZX_ERR_PEER_CLOSED);
  }
}

TransferStatus Tracee::WriteProviderIdRecord() const {
  if (provider_info_record_written_) {
    return output_->WriteProviderSectionRecord(bundle_->id);
  }
  auto status = output_->WriteProviderInfoRecord(bundle_->id, bundle_->name);
  provider_info_record_written_ = true;
  return status;
}

void Tracee::Abort() {
  FX_LOGS(ERROR) << *bundle_ << ": Aborting connection";
  Terminate();
}

const char* Tracee::ModeName(fuchsia::tracing::BufferingMode mode) {
  switch (mode) {
    case fuchsia::tracing::BufferingMode::ONESHOT:
      return "oneshot";
    case fuchsia::tracing::BufferingMode::CIRCULAR:
      return "circular";
    case fuchsia::tracing::BufferingMode::STREAMING:
      return "streaming";
  }
}

std::ostream& operator<<(std::ostream& out, Tracee::State state) {
  switch (state) {
    case Tracee::State::kReady:
      out << "ready";
      break;
    case Tracee::State::kInitialized:
      out << "initialized";
      break;
    case Tracee::State::kStarting:
      out << "starting";
      break;
    case Tracee::State::kStarted:
      out << "started";
      break;
    case Tracee::State::kStopping:
      out << "stopping";
      break;
    case Tracee::State::kStopped:
      out << "stopped";
      break;
    case Tracee::State::kTerminating:
      out << "terminating";
      break;
    case Tracee::State::kTerminated:
      out << "terminated";
      break;
  }

  return out;
}

}  // namespace tracing
