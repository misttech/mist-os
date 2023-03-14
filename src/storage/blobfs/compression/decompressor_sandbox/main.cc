// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/blobfs/internal/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/trace-provider/provider.h>

#include "src/storage/blobfs/compression/decompressor_sandbox/decompressor_impl.h"

int main(int argc, const char** argv) {
  async::Loop trace_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx_status_t status = trace_loop.StartThread();
  if (status != ZX_OK)
    exit(1);
  trace::TraceProviderWithFdio trace_provider(trace_loop.dispatcher());

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  blobfs::DecompressorImpl impl;
  fidl::BindingSet<fuchsia::blobfs::internal::DecompressorCreator> bindings;
  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();
  context->outgoing()->AddPublicService<fuchsia::blobfs::internal::DecompressorCreator>(
      bindings.GetHandler(&impl));

  return loop.Run();
}
