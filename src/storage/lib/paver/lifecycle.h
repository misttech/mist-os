// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_LIFECYCLE_H_
#define SRC_STORAGE_LIB_PAVER_LIFECYCLE_H_

#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>

namespace paver {

class LifecycleServer final : public fidl::WireServer<fuchsia_process_lifecycle::Lifecycle> {
 public:
  using FinishedCallback = fit::callback<void(zx_status_t status)>;
  using ShutdownCallback = fit::callback<void(FinishedCallback)>;

  explicit LifecycleServer(ShutdownCallback shutdown) : shutdown_(std::move(shutdown)) {}

  static void Create(async_dispatcher_t* dispatcher, ShutdownCallback shutdown,
                     fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> request);

  void Stop(StopCompleter::Sync& completer) override;

 private:
  ShutdownCallback shutdown_;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_LIFECYCLE_H_
