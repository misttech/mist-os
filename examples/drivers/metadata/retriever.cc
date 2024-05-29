// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>

#include "metadata.h"

class Retriever : public fdf::DriverBase {
 public:
  Retriever(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("child", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    zx::result<fuchsia_examples_metadata::Metadata> metadata =
        fdf_metadata::GetMetadata<fuchsia_examples_metadata::Metadata>(incoming());
    ZX_ASSERT(!metadata.is_error());

    return zx::ok();
  }
};

FUCHSIA_DRIVER_EXPORT(Retriever);
