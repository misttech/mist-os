// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "wlan/drivers/testing/test_helpers.h"

namespace wlan::drivers::log::testing {

UnitTestLogContext::UnitTestLogContext(std::string name) {
  loop_ = std::make_unique<::async::Loop>(&kAsyncLoopConfigNeverAttachToThread);
  ZX_ASSERT(ZX_OK == loop_->StartThread(std::string(name + "-worker").c_str(), nullptr));

  std::vector<fuchsia_component_runner::ComponentNamespaceEntry> entries;
  zx::result open_result = component::OpenServiceRoot();
  ZX_ASSERT(open_result.is_ok());

  ::fidl::ClientEnd<::fuchsia_io::Directory> svc = std::move(*open_result);
  entries.emplace_back(fuchsia_component_runner::ComponentNamespaceEntry{{
      .path = "/svc",
      .directory = std::move(svc),
  }});

  // Create Namespace object from the entries.
  auto ns = fdf::Namespace::Create(entries);
  ZX_ASSERT(ns.is_ok());

  // Create Logger with dispatcher and namespace.
  auto logger = fdf::Logger::Create(*ns, loop_->dispatcher(), name + "-logger");
  ZX_ASSERT(logger.is_ok());

  logger_ = std::move(logger.value());

  fdf::Logger::SetGlobalInstance(logger_.get());
}

UnitTestLogContext::~UnitTestLogContext() { fdf::Logger::SetGlobalInstance(nullptr); }

}  // namespace wlan::drivers::log::testing
