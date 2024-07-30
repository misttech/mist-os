// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_PROVIDER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_PROVIDER_H_

#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/zx/result.h>

namespace display {

// Interface for a component that exposes Sysmem in its outgoing directory.
//
// Sysmem is exposed using the [`fuchsia.hardware.sysmem/Service`] FIDL
// interface.
class SysmemServiceProvider {
 public:
  SysmemServiceProvider() = default;
  virtual ~SysmemServiceProvider() = default;

  SysmemServiceProvider(const SysmemServiceProvider&) = delete;
  SysmemServiceProvider& operator=(const SysmemServiceProvider&) = delete;
  SysmemServiceProvider(SysmemServiceProvider&&) = delete;
  SysmemServiceProvider& operator=(SysmemServiceProvider&&) = delete;

  // Returns the component's outgoing directory.
  //
  // The returned directory is guaranteed to have a `svc/` subdirectory that
  // serves a [`fuchsia.hardware.sysmem/Service`] interface.
  virtual zx::result<fidl::ClientEnd<fuchsia_io::Directory>> GetOutgoingDirectory() = 0;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_FAKE_SYSMEM_SERVICE_PROVIDER_H_
