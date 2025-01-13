// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/fake-sysmem-device-hierarchy.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/sync/cpp/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <memory>
#include <utility>

#include "src/sysmem/server/allocator.h"
#include "src/sysmem/server/sysmem.h"

namespace fake_display {

zx::result<std::unique_ptr<FakeSysmemDeviceHierarchy>> FakeSysmemDeviceHierarchy::Create() {
  return zx::ok(std::make_unique<FakeSysmemDeviceHierarchy>());
}

FakeSysmemDeviceHierarchy::FakeSysmemDeviceHierarchy()
    : loop_(&kAsyncLoopConfigNeverAttachToThread) {
  zx_status_t start_status = loop_.StartThread("FakeSysmemDeviceHierarchy");
  ZX_ASSERT_MSG(start_status == ZX_OK, "loop_.StartThread failed: %s",
                zx_status_get_string(start_status));

  libsync::Completion done;
  zx_status_t post_status = async::PostTask(loop_.dispatcher(), [this, &done] {
    sysmem_service::Sysmem::CreateArgs create_args;
    zx::result<std::unique_ptr<sysmem_service::Sysmem>> create_result =
        sysmem_service::Sysmem::Create(loop_.dispatcher(), create_args);
    ZX_ASSERT_MSG(create_result.is_ok(), "sysmem_service::Sysmem::Create() failed: %s",
                  create_result.status_string());
    sysmem_service_ = std::move(create_result.value());
    done.Signal();
  });
  ZX_ASSERT(post_status == ZX_OK);
  done.Wait();
}

zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>>
FakeSysmemDeviceHierarchy::ConnectAllocator2() {
  auto [client, server] = fidl::Endpoints<fuchsia_sysmem2::Allocator>::Create();
  sysmem_service_->SyncCall([this, request = std::move(server)]() mutable {
    sysmem_service::Allocator::CreateOwnedV2(std::move(request), sysmem_service_.get(),
                                             sysmem_service_->v2_allocators());
  });
  return zx::ok(std::move(client));
}

zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>>
FakeSysmemDeviceHierarchy::ConnectHardwareSysmem() {
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_sysmem::Sysmem>::Create();
  // The loop_ dispatcher is the "client_dispatcher" in sysmem_service_.
  async::PostTask(loop_.dispatcher(), [this, server = std::move(server)]() mutable {
    sysmem_service_->BindingsForTest().AddBinding(
        loop_.dispatcher(), std::move(server), sysmem_service_.get(), fidl::kIgnoreBindingClosure);
  });
  return zx::ok(std::move(client));
}

FakeSysmemDeviceHierarchy::~FakeSysmemDeviceHierarchy() {
  // ensure this runs first regardless of field order
  libsync::Completion done;
  zx_status_t post_status = async::PostTask(loop_.dispatcher(), [this, &done] {
    sysmem_service_.reset();
    done.Signal();
  });
  ZX_ASSERT(post_status == ZX_OK);
  done.Wait();
}

}  // namespace fake_display
