// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon_platform_trace_provider.h"

#include <lib/async/cpp/task.h>
#include <lib/magma/util/dlog.h>
#include <lib/magma/util/short_macros.h>
#include <zircon/syscalls.h>

#include <memory>

namespace magma {

static std::unique_ptr<ZirconPlatformTraceProvider> g_platform_trace;

ZirconPlatformTraceProvider::ZirconPlatformTraceProvider()
    : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

ZirconPlatformTraceProvider::~ZirconPlatformTraceProvider() {
  if (trace_provider_) {
    async::PostTask(loop_.dispatcher(), [this]() {
      // trace_provider_.reset() needs to run on loop_'s dispatcher or else its teardown can be
      // racy and crash.
      trace_provider_.reset();
      // Run Quit() in the loop to ensure this task executes before JoinThreads() returns and the
      // destructor finishes.
      loop_.Quit();
    });
  } else {
    loop_.Quit();
  }
  loop_.JoinThreads();
}

bool ZirconPlatformTraceProvider::Initialize(uint32_t channel) {
  zx::channel zx_channel(channel);
  zx_status_t status = loop_.StartThread("magma trace provider");
  if (status != ZX_OK)
    return DRETF(false, "Failed to start async loop");
  trace_provider_ =
      std::make_unique<trace::TraceProvider>(std::move(zx_channel), loop_.dispatcher());
  return true;
}

// static
PlatformTraceProvider* PlatformTraceProvider::Get() {
  if (!g_platform_trace)
    g_platform_trace = std::make_unique<ZirconPlatformTraceProvider>();
  return g_platform_trace.get();
}

// static
void PlatformTraceProvider::Shutdown() { g_platform_trace.reset(); }

// static
std::unique_ptr<PlatformTraceProvider> PlatformTraceProvider::CreateForTesting() {
  return std::make_unique<ZirconPlatformTraceProvider>();
}

}  // namespace magma
