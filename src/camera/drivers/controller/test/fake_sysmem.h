// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CAMERA_DRIVERS_CONTROLLER_TEST_FAKE_SYSMEM_H_
#define SRC_CAMERA_DRIVERS_CONTROLLER_TEST_FAKE_SYSMEM_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire_test_base.h>
#include <fuchsia/sysmem/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/sys/cpp/component_context.h>

#include "fidl/fuchsia.hardware.sysmem/cpp/markers.h"

class FakeSysmem : public fidl::testing::WireTestBase<fuchsia_hardware_sysmem::Sysmem> {
 public:
  void ConnectServer(ConnectServerRequestView request,
                     ConnectServerCompleter::Sync& completer) override {}

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fuchsia_hardware_sysmem::Service::InstanceHandler CreateInstanceHandler() {
    return fuchsia_hardware_sysmem::Service::InstanceHandler({
        .sysmem = sysmem_bindings_.CreateHandler(this, async_get_default_dispatcher(),
                                                 fidl::kIgnoreBindingClosure),
        .allocator_v1 = [](fidl::ServerEnd<fuchsia_sysmem::Allocator> request) {},
        .allocator = [](fidl::ServerEnd<fuchsia_sysmem2::Allocator> request) {},
    });
  }

  fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem> Connect() {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_sysmem::Sysmem>();
    ZX_ASSERT(endpoints.is_ok());
    sysmem_bindings_.AddBinding(async_get_default_dispatcher(), std::move(endpoints->server), this,
                                fidl::kIgnoreBindingClosure);
    return std::move(endpoints->client);
  }

 private:
  fidl::ServerBindingGroup<fuchsia_hardware_sysmem::Sysmem> sysmem_bindings_;
};

#endif  // SRC_CAMERA_DRIVERS_CONTROLLER_TEST_FAKE_SYSMEM_H_
