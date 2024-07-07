// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/sync/cpp/completion.h>

namespace fdf_testing {

ScopedGlobalLogger::ScopedGlobalLogger() : loop_(&kAsyncLoopConfigNeverAttachToThread) {
  ZX_ASSERT_MSG(!fdf::Logger::HasGlobalInstance(), "There is already an active logger.");

  const std::string loop_thread_name = "fdf-testing-scoped-global-logger-dispatcher-thread";
  zx_status_t status = loop_.StartThread(loop_thread_name.c_str(), nullptr);
  ZX_ASSERT(ZX_OK == status);

  zx::result open_result = component::OpenServiceRoot();
  ZX_ASSERT(open_result.is_ok());

  std::vector<fuchsia_component_runner::ComponentNamespaceEntry> entries;
  fidl::ClientEnd<fuchsia_io::Directory> svc = std::move(open_result).value();
  entries.emplace_back(fuchsia_component_runner::ComponentNamespaceEntry{{
      .path = "/svc",
      .directory = std::move(svc),
  }});

  // Create Namespace object from the entries.
  zx::result<fdf::Namespace> ns_result = fdf::Namespace::Create(entries);
  ZX_ASSERT(ns_result.is_ok());

  // Create Logger with dispatcher and namespace.
  // Must post to the dispatcher.
  libsync::Completion completion;
  async::PostTask(loop_.dispatcher(), [&]() {
    zx::result<std::unique_ptr<fdf::Logger>> logger = fdf::Logger::Create(
        std::move(ns_result).value(), loop_.dispatcher(), "fdf-testing-scoped-global-logger");
    ZX_ASSERT(logger.is_ok());

    logger_ = std::move(logger).value();
    completion.Signal();
  });
  completion.Wait();
  fdf::Logger::SetGlobalInstance(logger_.get());
}

ScopedGlobalLogger::~ScopedGlobalLogger() {
  fdf::Logger::SetGlobalInstance(nullptr);

  // Destroy the logger. Must post to the dispatcher.
  libsync::Completion completion;
  async::PostTask(loop_.dispatcher(), [this, &completion]() {
    logger_.reset();
    completion.Signal();
  });
  completion.Wait();
}

}  // namespace fdf_testing
