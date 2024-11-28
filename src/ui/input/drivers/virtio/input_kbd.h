// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_KBD_H_
#define SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_KBD_H_

#include "input_device.h"

namespace virtio {

static constexpr int kMaxKeys = 6;
struct KeyboardReport {
  zx::time event_time = zx::time(ZX_TIME_INFINITE_PAST);
  std::array<std::optional<fuchsia_input::wire::Key>, kMaxKeys> usage;

  void ToFidlInputReport(
      fidl::WireTableBuilder<::fuchsia_input_report::wire::InputReport>& input_report,
      fidl::AnyArena& allocator) const;
};

class HidKeyboard : public HidDevice<KeyboardReport> {
 public:
  fuchsia_input_report::wire::DeviceDescriptor GetDescriptor(fidl::AnyArena& allocator) override;
  void ReceiveEvent(virtio_input_event_t* event) override;

 private:
  void AddKeypressToReport(uint16_t event_code);
  void RemoveKeypressFromReport(uint16_t event_code);
};

}  // namespace virtio

#endif  // SRC_UI_INPUT_DRIVERS_VIRTIO_INPUT_KBD_H_
