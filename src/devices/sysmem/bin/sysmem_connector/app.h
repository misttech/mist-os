// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_BIN_SYSMEM_CONNECTOR_APP_H_
#define SRC_DEVICES_SYSMEM_BIN_SYSMEM_CONNECTOR_APP_H_

#include <memory>

#include "src/devices/sysmem/drivers/sysmem/sysmem.h"
#include "src/lib/fxl/macros.h"

class App {
 public:
  explicit App(async_dispatcher_t* dispatcher);
  ~App();

 private:
  async_dispatcher_t* dispatcher_ = nullptr;

  std::unique_ptr<sysmem_service::Sysmem> device_;

  FXL_DISALLOW_COPY_AND_ASSIGN(App);
};

#endif  // SRC_DEVICES_SYSMEM_BIN_SYSMEM_CONNECTOR_APP_H_
