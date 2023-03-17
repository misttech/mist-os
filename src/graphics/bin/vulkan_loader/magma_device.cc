// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/bin/vulkan_loader/magma_device.h"

#include <lib/fdio/directory.h>
#include <lib/fit/thread_checker.h>
#include <lib/syslog/cpp/macros.h>

#include "src/graphics/bin/vulkan_loader/app.h"

// static
std::unique_ptr<MagmaDevice> MagmaDevice::Create(LoaderApp* app,
                                                 const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                                 const std::string& name, inspect::Node* parent) {
  std::unique_ptr<MagmaDevice> device(new MagmaDevice(app));
  if (!device->Initialize(dir, name, parent))
    return nullptr;
  return device;
}

bool MagmaDevice::Initialize(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                             const std::string& name, inspect::Node* parent) {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);
  node() = parent->CreateChild("magma-" + name);
  icd_list_.Initialize(&node());
  auto pending_action_token = app()->GetPendingActionToken();

  zx_status_t status = fdio_service_connect_at(dir.channel().get(), name.c_str(),
                                               device_.NewRequest().TakeChannel().release());
  if (status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to connect to service";
    return false;
  }
  device_.set_error_handler([this](zx_status_t status) {
    // Deletes |this|.
    app()->RemoveDevice(this);
  });

  device_->GetIcdList([this, pending_action_token = std::move(pending_action_token)](
                          const std::vector<fuchsia::gpu::magma::IcdInfo>& icd_info) mutable {
    FIT_DCHECK_IS_THREAD_VALID(main_thread_);
    uint32_t i = 0;
    for (auto& icd : icd_info) {
      if (!icd.has_component_url()) {
        FX_LOGS(ERROR) << "ICD missing component URL";
        continue;
      }
      if (!icd.has_flags()) {
        FX_LOGS(ERROR) << "ICD missing flags";
        continue;
      }
      auto data = node().CreateChild(std::to_string(i++));
      data.RecordString("component_url", icd.component_url());
      data.RecordUint("flags", static_cast<uint32_t>(icd.flags()));
      if (icd.flags() & fuchsia::gpu::magma::IcdFlags::SUPPORTS_VULKAN) {
        icd_list_.Add(app()->CreateIcdComponent(icd.component_url()));
      }

      icds().push_back(std::move(data));
    }
  });
  return true;
}
