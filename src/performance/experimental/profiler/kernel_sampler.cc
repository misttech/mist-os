// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "kernel_sampler.h"

#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fit/defer.h>
#include <zircon/syscalls-next.h>
#include <zircon/syscalls.h>

#include <trace-reader/reader.h>
#include <trace-reader/records.h>

zx::result<std::unique_ptr<profiler::KernelSamplerSession>>
profiler::KernelSamplerSession::CreateAndInit(const zx_sampler_config_t& config) {
  auto debug_client_end = component::Connect<fuchsia_kernel::DebugResource>();
  if (debug_client_end.is_error()) {
    FX_PLOGS(ERROR, debug_client_end.error_value()) << "Failed to get connect to debug resource";
    return zx::error(debug_client_end.status_value());
  }
  auto debug_result = fidl::SyncClient(std::move(*debug_client_end))->Get();
  if (!debug_result.is_ok()) {
    FX_LOGS(ERROR) << debug_result.error_value() << " Failed to get debug resource";
    return zx::error(debug_result.error_value().status());
  }

  zx::resource debug_resource = std::move(debug_result->resource());

  zx::iob iob;

  if (zx_status_t init_status =
          zx_sampler_create(debug_resource.get(), 0, &config, iob.reset_and_get_address());
      init_status != ZX_OK) {
    return zx::error(init_status);
  }

  return zx::ok(std::make_unique<profiler::KernelSamplerSession>(std::move(iob)));
}

zx::result<> profiler::KernelSamplerSession::Start() {
  if (running_) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  running_ = true;
  return zx::make_result(zx_sampler_start(per_cpu_buffers_.get()));
}

zx::result<> profiler::KernelSamplerSession::Stop() {
  if (!running_) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  running_ = false;
  return zx::make_result(zx_sampler_stop(per_cpu_buffers_.get()));
}

zx::result<> profiler::KernelSamplerSession::AttachThread(const zx::thread& thread) const {
  FX_LOGS(INFO) << "Attaching to thread: " << thread.get();
  return zx::make_result(zx_sampler_attach(per_cpu_buffers_.get(), thread.get()));
}

zx::result<> profiler::KernelSampler::Start(size_t buffer_size_mb) {
  buffer_size_bytes_ = (1 << 20) * buffer_size_mb;
  zx_sampler_config_t config{.period = zx::msec(10).get(), .buffer_size = buffer_size_bytes_};
  zx::result session_result = KernelSamplerSession::CreateAndInit(config);
  if (session_result.is_error()) {
    return session_result.take_error();
  }
  session_ = std::move(session_result).value();

  zx::result known_threads_res = targets_.ForEachProcess(
      [this](cpp20::span<const zx_koid_t> job_path, const ProcessTarget& p) -> zx::result<> {
        for (const auto& [koid, thread] : p.threads) {
          if (zx::result res = session_->AttachThread(thread.handle); res.is_error()) {
            FX_PLOGS(ERROR, res.error_value()) << "failed to set up thread sampling";
            return res;
          }
        }

        std::vector<const zx_koid_t> saved_path{job_path.begin(), job_path.end()};
        auto process_watcher = std::make_unique<ProcessWatcher>(
            p.handle.borrow(),
            [saved_path, this](zx_koid_t pid, zx_koid_t tid, zx::thread t) {
              AddThread(saved_path, pid, tid, std::move(t));
            },
            [saved_path, this](zx_koid_t pid, zx_koid_t tid) {
              RemoveThread(saved_path, pid, tid);
            });

        auto [it, emplaced] = process_watchers_.emplace(p.pid, std::move(process_watcher));
        if (emplaced) {
          zx::result watch_result = it->second->Watch(dispatcher_);
          if (watch_result.is_error()) {
            FX_PLOGS(ERROR, watch_result.status_value()) << "Failed to watch process: " << p.pid;
            job_watchers_.clear();
            process_watchers_.clear();
            return watch_result.take_error();
          }
        }
        return zx::ok();
      });
  if (known_threads_res.is_error()) {
    return known_threads_res;
  }

  // If a watched job launches a new process, we want to add it to the set
  zx::result watch_result =
      targets_.ForEachJob([this](const JobTarget& target) { return WatchTarget(target); });
  if (watch_result.is_error()) {
    return watch_result;
  }
  return session_->Start();
}

zx::result<> profiler::KernelSampler::AddTarget(JobTarget&& target) {
  if (session_) {
    if (zx::result<> watch_res = WatchTarget(target); watch_res.is_error()) {
      return watch_res;
    }
    zx::result<> res = target.ForEachProcess(
        [this](cpp20::span<const zx_koid_t> job_path, const ProcessTarget& p) -> zx::result<> {
          for (const auto& [koid, thread] : p.threads) {
            if (zx::result res = session_->AttachThread(thread.handle); res.is_error()) {
              FX_PLOGS(ERROR, res.error_value()) << "failed Add thread target";
              return res;
            }
          }
          return zx::ok();
        });
    if (res.is_error()) {
      return res;
    }
  }
  return targets_.AddJob(std::move(target));
}

zx::result<> profiler::KernelSampler::Stop() {
  if (zx::result res = session_->Stop(); res.is_error()) {
    FX_PLOGS(WARNING, res.error_value()) << "Failed to stop";
    return res;
  }
  zx::iob buffers;
  if (zx::result buffers_result = session_->GetBuffers(); buffers_result.is_ok()) {
    buffers = *std::move(buffers_result);
  } else {
    return buffers_result.take_error();
  }
  session_.reset();

  zx_info_iob_t info;
  zx_status_t res = buffers.get_info(ZX_INFO_IOB, &info, sizeof(info), nullptr, nullptr);
  if (res != ZX_OK) {
    FX_PLOGS(ERROR, res) << "Failed to get info about iob";
    return zx::error(res);
  }

  trace::TraceReader::RecordConsumer consume_record = [this](trace::Record rec) {
    if (rec.type() != trace::RecordType::kLargeRecord) {
      FX_LOGS(WARNING) << "Unhandled record type: " << static_cast<uint64_t>(rec.type());
      return;
    }
    const trace::LargeRecordData& large_record = rec.GetLargeRecord();
    if (large_record.type() != trace::LargeRecordType::kBlob) {
      FX_LOGS(WARNING) << "Unhandled large record type: "
                       << static_cast<uint64_t>(large_record.type());
      return;
    }

    const trace::LargeRecordData::Blob& blob = large_record.GetBlob();
    if (std::holds_alternative<trace::LargeRecordData::BlobAttachment>(blob)) {
      FX_LOGS(WARNING) << "Unhandled large blob without metadata";
      return;
    }

    const trace::LargeRecordData::BlobEvent& blob_event =
        std::get<trace::LargeRecordData::BlobEvent>(blob);
    // The blob we are given is an array of instruction pointers of size blob_size
    const uint64_t* read_head = reinterpret_cast<const uint64_t*>(blob_event.blob);
    const std::vector<uint64_t> stack{read_head,
                                      read_head + blob_event.blob_size / sizeof(uint64_t)};
    const zx_koid_t pid = blob_event.process_thread.process_koid();
    const zx_koid_t tid = blob_event.process_thread.thread_koid();
    samples_[pid].push_back({pid, tid, std::move(stack)});

    // TODO(gmtr) figure out how properly measure the overhead of kernel sampling
    inspecting_durations_.emplace_back(0);
  };
  zx_status_t encountered_error = ZX_OK;
  trace::TraceReader::ErrorHandler handle_error = [&encountered_error](fbl::String err) {
    FX_LOGS(ERROR) << "Encountered malformed data: " << err.c_str();
    encountered_error = ZX_ERR_BAD_STATE;
  };
  trace::TraceReader reader{std::move(consume_record), std::move(handle_error)};

  for (uint32_t i = 0; i < info.region_count; ++i) {
    zx_vaddr_t ptr;
    zx_status_t res =
        zx::vmar::root_self()->map_iob(ZX_VM_PERM_READ, 0, buffers, i, 0, buffer_size_bytes_, &ptr);

    if (res != ZX_OK) {
      FX_PLOGS(INFO, res) << "Failed to map region";
      return zx::error(res);
    }
    trace::Chunk data{reinterpret_cast<uint64_t*>(ptr), buffer_size_bytes_ / 8};
    if (!reader.ReadRecords(data)) {
      FX_LOGS(ERROR) << "Buffer " << i << " corrupted";
      encountered_error = ZX_ERR_BAD_STATE;
    }
    if (zx_status_t status = zx::vmar::root_self()->unmap(ptr, buffer_size_bytes_);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to unmap buffer " << i;
      return zx::error(status);
    }
  }

  return zx::make_result(encountered_error);
}

void profiler::KernelSampler::AddThread(std::vector<const zx_koid_t> job_path, zx_koid_t pid,
                                        zx_koid_t tid, zx::thread t) {
  if (zx::result res = session_->AttachThread(t); res.is_error()) {
    FX_PLOGS(ERROR, res.status_value()) << "Failed to start sampling thread: " << tid;
  }
  // Add the the thread it so we can later grab its address space and module information for
  // symbolization purposes.
  if (zx::result res =
          targets_.AddThread(job_path, pid, ThreadTarget{.handle = std::move(t), .tid = tid});
      res.is_error()) {
    FX_PLOGS(ERROR, res.status_value()) << "Failed to add thread to session: " << tid;
  }
}

void profiler::KernelSampler::RemoveThread(std::vector<const zx_koid_t> job_path, zx_koid_t pid,
                                           zx_koid_t tid) {
  zx::result res = targets_.RemoveThread(job_path, pid, tid);
  if (res.is_error()) {
    FX_PLOGS(ERROR, res.status_value()) << "Failed to remove exited thread: " << tid;
  }
}
