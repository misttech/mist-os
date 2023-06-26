// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_DRIVERS_AMLOGIC_DECODER_TEST_DEVICE_CTX_H_
#define SRC_MEDIA_DRIVERS_AMLOGIC_DECODER_TEST_DEVICE_CTX_H_

#include <fidl/fuchsia.hardware.mediacodec/cpp/wire.h>

#include <ddktl/device.h>

namespace amlogic_decoder::test {

class AmlogicTestDevice;
using DdkDeviceType =
    ddk::Device<AmlogicTestDevice, ddk::Messageable<fuchsia_hardware_mediacodec::Tester>::Mixin>;

class AmlogicTestDevice : public DdkDeviceType {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  explicit AmlogicTestDevice(zx_device_t* parent) : DdkDeviceType(parent) {}
  zx_status_t Bind();

  void DdkRelease();

  void SetOutputDirectoryHandle(SetOutputDirectoryHandleRequestView request,
                                SetOutputDirectoryHandleCompleter::Sync& completer) override;
  void RunTests(RunTestsCompleter::Sync& completer) override;
};

}  // namespace amlogic_decoder::test

#endif  // SRC_MEDIA_DRIVERS_AMLOGIC_DECODER_TEST_DEVICE_CTX_H_
