// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test_device_ctx.h"

#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>

#include <memory>

#include "macros.h"
#include "tests/test_support.h"

namespace amlogic_decoder::test {

zx_status_t AmlogicTestDevice::Create(void* ctx, zx_device_t* parent) {
  auto test_device = std::make_unique<amlogic_decoder::test::AmlogicTestDevice>(parent);

  if (test_device->Bind() != ZX_OK) {
    return ZX_ERR_INTERNAL;
  }
  test_device.release();
  return ZX_OK;
}

zx_status_t AmlogicTestDevice::Bind() { return DdkAdd("test_amlogic_video"); }

void AmlogicTestDevice::DdkRelease() { delete this; }

void AmlogicTestDevice::SetOutputDirectoryHandle(
    SetOutputDirectoryHandleRequestView request,
    SetOutputDirectoryHandleCompleter::Sync& completer) {
  fdio_ns_t* ns;
  zx_status_t status = fdio_ns_get_installed(&ns);
  status = fdio_ns_bind(ns, "/tmp", request->handle.release());
  fprintf(stderr, "NS bind: %d\n", status);
}

void AmlogicTestDevice::RunTests(RunTestsCompleter::Sync& completer) {
  TestSupport::set_parent_device(parent());
  if (!TestSupport::RunAllTests()) {
    DECODE_ERROR("Tests failed, failing to initialize");
    completer.Reply(ZX_ERR_INTERNAL);
  } else {
    completer.Reply(ZX_OK);
  }
}

}  // namespace amlogic_decoder::test
