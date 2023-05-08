// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sampler.h"

#include <lib/zx/suspend_token.h>

std::pair<zx::ticks, std::vector<uint64_t>> SampleThread(const zx::process& process,
                                                         const zx::thread& thread,
                                                         unwinder::FramePointerUnwinder& unwinder) {
  zx_info_thread_t thread_info;
  zx_status_t status =
      thread.get_info(ZX_INFO_THREAD, &thread_info, sizeof(thread_info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "unable to get thread info for thread " << thread.get()
                            << ", skipping";
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  if (thread_info.state != ZX_THREAD_STATE_RUNNING) {
    // Skip blocked threads, they don't count as work...
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  zx::ticks before = zx::ticks::now();
  zx::suspend_token suspend_token;
  status = thread.suspend(&suspend_token);
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to suspend thread: " << thread.get();
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  // Asking to wait for suspended means only waiting for the thread to suspend. If the thread
  // terminates instead this will wait forever (or until the timeout). Thus we need to explicitly
  // wait for ZX_THREAD_TERMINATED too.
  zx_signals_t signals = ZX_THREAD_SUSPENDED | ZX_THREAD_TERMINATED;
  zx_signals_t observed = 0;
  zx::time deadline = zx::deadline_after(zx::msec(100));
  status = thread.wait_one(signals, deadline, &observed);

  if (status != ZX_OK) {
    FX_PLOGS(WARNING, status) << "failure waiting for thread to suspend, skipping thread: "
                              << thread.get();
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }

  if (observed & ZX_THREAD_TERMINATED) {
    return {zx::ticks(), std::vector<uint64_t>()};  // Skip this thread.
  }
  unwinder::FuchsiaMemory memory(process.get());

  // Setup registers.
  zx_thread_state_general_regs_t regs;
  if (thread.read_state(ZX_THREAD_STATE_GENERAL_REGS, &regs, sizeof(regs)) != ZX_OK) {
    return {zx::ticks(), std::vector<uint64_t>()};
  }
  auto registers = unwinder::FromFuchsiaRegisters(regs);

  std::vector<uint64_t> pcs;
  pcs.reserve(50);
  registers.GetPC(pcs.emplace_back());
  unwinder::Frame current{registers, unwinder::Frame::Trust::kContext};
  for (size_t i = 0; i < 50; i++) {
    unwinder::Frame next(unwinder::Registers{current.regs.arch()},
                         /*placeholder*/ unwinder::Frame::Trust::kFP);

    bool success = unwinder.Step(&memory, current.regs, next.regs).ok();

    // An undefined PC (e.g. on Linux) or 0 PC (e.g. on Fuchsia) marks the end of the unwinding.
    // Don't include this in the output because it's not a real frame and provides no information.
    // A failed unwinding will also end up with an undefined PC.
    uint64_t pc;
    if (!success || next.regs.GetPC(pc).has_err() || pc == 0) {
      break;
    }
    pcs.push_back(pc);
    current = next;
  }
  zx::ticks duration = zx::ticks::now() - before;
  return {duration, pcs};
}

zx::result<> Sampler::Start() {
  {
    std::lock_guard lock(state_lock_);
    if (state_ != State::Stopped) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    state_ = State::Running;
  }
  std::lock_guard data_lock(data_lock_);
  collection_thread_ = std::thread([this]() mutable { CollectSamples(); });
  collection_thread_.detach();
  return zx::ok();
}

zx::result<> Sampler::Stop() {
  {
    std::unique_lock lock(state_lock_);
    if (state_ == State::Stopped) {
      return zx::ok();
    }
    state_ = State::Stopping;
    state_cv_.wait(lock, [this]() { return state_ == State::Stopped; });
  }
  std::lock_guard data_lock(data_lock_);
  FX_LOGS(INFO) << "Stopped! Collected " << stacks_.size() << " samples";
  return zx::ok();
}

void Sampler::CollectSamples() {
  std::lock_guard data_lock(data_lock_);
  inspecting_durations_.reserve(1000);
  stacks_.reserve(1000);

  FX_LOGS(INFO) << "Main Sampling Loop";
  while (state_.load() == State::Running) {
    for (SamplingInfo& target : targets_) {
      for (const zx::thread& thread : target.threads) {
        auto [time_sampling, pcs] = SampleThread(target.process, thread, target.fp_unwinder);
        if (time_sampling != zx::ticks()) {
          stacks_.push_back(pcs);
          inspecting_durations_.push_back(time_sampling);
        }
      }
    }
    zx::nanosleep(zx::deadline_after(zx::msec(10)));
  }
  {
    std::lock_guard lock(state_lock_);
    state_ = State::Stopped;
    state_cv_.notify_all();
  }
  FX_LOGS(INFO) << "Done profiling";
}
