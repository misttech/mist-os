// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tracing.h"

#include <lib/fxt/fields.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zircon-internal/ktrace.h>

#include <fstream>

#include <trace-reader/reader.h>

// Performs same action as zx_ktrace_read and does necessary checks.
void Tracing::ReadKernelBuffer(zx_handle_t handle, void* data_buf, uint32_t offset, size_t len,
                               size_t* bytes_read) {
  const zx_status_t status = zx_ktrace_read(handle, data_buf, offset, len, bytes_read);
  if (status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx_trace_read";
  }
}

// Rewinds kernel trace buffer.
void Tracing::Rewind() {
  const zx_status_t status = zx_ktrace_control(tracing_resource_, KTRACE_ACTION_REWIND, 0, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx_ktrace_control(_, KTRACE_ACTION_REWIND, _, _)";
  }
}

// Starts kernel tracing.
void Tracing::Start(uint32_t group_mask) {
  const zx_status_t status =
      zx_ktrace_control(tracing_resource_, KTRACE_ACTION_START, group_mask, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx_ktrace_control(_, KTRACE_ACTION_START, _, _)";
  }

  running_ = true;
}

// Stops kernel tracing.
void Tracing::Stop() {
  const zx_status_t status = zx_ktrace_control(tracing_resource_, KTRACE_ACTION_STOP, 0, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx_ktrace_control(_, KTRACE_ACTION_STOP, _, _)";
  }

  running_ = false;
}

size_t Tracing::ReadKernelRecords(trace::TraceReader::RecordConsumer consumer,
                                  trace::TraceReader::ErrorHandler error_handler) {
  trace::TraceReader reader(std::move(consumer), std::move(error_handler));
  size_t bytes_read = 0;

  uint8_t buffer[4096];
  size_t available_bytes = 0;
  ReadKernelBuffer(tracing_resource_, nullptr, 0, 0, &available_bytes);

  size_t bytes_processed = 0;
  while (bytes_processed < available_bytes) {
    ReadKernelBuffer(tracing_resource_, buffer, static_cast<uint32_t>(bytes_processed),
                     sizeof(buffer), &bytes_read);
    if (bytes_read < 8) {
      break;
    }
    trace::Chunk chunk{reinterpret_cast<uint64_t*>(buffer), bytes_read / 8};
    reader.ReadRecords(chunk);
    bytes_processed += bytes_read - (chunk.remaining_words() * 8);
  }

  return bytes_processed;
}

// Reads trace buffer and converts output into human-readable format. Stores in location defined by
// <filepath>. Will overwrite any existing files with same name.
bool Tracing::WriteHumanReadable(std::ostream& filepath) {
  if (running_) {
    FX_LOGS(WARNING) << "Tracing was running when human readable translation was started. Tracing "
                        "stopped.";
    Stop();
  }

  if (!filepath) {
    FX_LOGS(ERROR) << "Failed to open file.";
    return false;
  }

  size_t record_count = 0;
  size_t error_count = 0;

  trace::TraceReader::RecordConsumer accumulate_records = [&record_count,
                                                           &filepath](trace::Record rec) {
    filepath << rec.ToString() << "\n";
    record_count++;
  };
  trace::TraceReader::ErrorHandler accumulate_errors = [&error_count](const fbl::String& error) {
    FX_LOGS(ERROR) << error << "\n";
    error_count++;
  };
  size_t bytes_read =
      ReadKernelRecords(std::move(accumulate_records), std::move(accumulate_errors));

  filepath << "\nTotal records read: " << std::dec << record_count
           << "\nTotal bytes read: " << std::dec << bytes_read << "\n";

  return error_count == 0;
}

// Picks out traces pertaining to name in string_ref and populates stats on them. Returns false if
// name not found.
bool Tracing::PopulateDurationStats(std::string string_ref,
                                    std::vector<DurationStats>* duration_stats,
                                    std::map<uint64_t, QueuingStats>* queuing_stats) {
  if (running_) {
    FX_LOGS(WARNING) << "Tracing was running when duration stats were started. Tracing stopped.";
    Stop();
  }

  size_t error_count = 0;
  bool string_ref_found = false;

  trace::TraceReader::RecordConsumer accumulate_records = [duration_stats, queuing_stats,
                                                           &string_ref_found,
                                                           &string_ref](trace::Record record) {
    auto name = record.GetName();
    if (name && *name == string_ref) {
      string_ref_found = true;
      // Match duration records for given string ref.
      if (record.type() == trace::RecordType::kEvent) {
        auto& event = record.GetEvent();
        if (event.type() == trace::EventType::kDurationComplete) {
          std::vector<trace::Argument> args;
          for (auto&& argument : event.arguments) {
            args.emplace_back(std::move(argument));
          }
          duration_stats->emplace_back(event.timestamp, event.data.GetDurationComplete().end_time,
                                       event.data.GetDurationComplete().end_time - event.timestamp,
                                       std::move(args));
        } else if (event.type() == trace::EventType::kDurationBegin) {
          duration_stats->emplace_back(event.timestamp);
        } else if (event.type() == trace::EventType::kDurationEnd) {
          for (auto duration = duration_stats->rbegin(); duration != duration_stats->rend();
               duration++) {
            if (duration->end_ts_ns == 0) {
              duration->end_ts_ns = event.timestamp;
              duration->wall_duration_ns = event.timestamp - duration->begin_ts_ns;
              for (auto&& argument : event.arguments) {
                duration->arguments.emplace_back(std::move(argument));
              }
              break;
            }
          }
        } else if (event.type() == trace::EventType::kFlowBegin) {
          auto [it, inserted] = queuing_stats->emplace(
              event.data.GetFlowBegin().id,
              QueuingStats(event.timestamp, event.process_thread.thread_koid()));
          if (!inserted) {
            FX_LOGS(ERROR) << "Failed to insert flow event of id: " << event.data.GetFlowBegin().id
                           << ". Id already exists";
          }
        } else if (event.type() == trace::EventType::kFlowEnd) {
          auto flow_iter = queuing_stats->find(event.data.GetFlowEnd().id);
          if (flow_iter == queuing_stats->end()) {
            return;
          }
          flow_iter->second.end_ts_ns = event.timestamp;
          flow_iter->second.queuing_time_ns =
              flow_iter->second.end_ts_ns - flow_iter->second.begin_ts_ns;
        }
      }
    }
  };
  trace::TraceReader::ErrorHandler accumulate_errors = [&error_count](const fbl::String& error) {
    FX_LOGS(ERROR) << error << "\n";
    error_count++;
  };
  ReadKernelRecords(std::move(accumulate_records), std::move(accumulate_errors));
  return string_ref_found && error_count == 0;
}
