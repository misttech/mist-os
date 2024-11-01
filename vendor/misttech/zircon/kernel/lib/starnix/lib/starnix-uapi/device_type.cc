// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos//starnix_uapi/device_type.h"

namespace starnix_uapi {

const DeviceType DeviceType::NONE(0);
const DeviceType DeviceType::_NULL = DeviceType::New(MEM_MAJOR, 3);
const DeviceType DeviceType::ZERO = DeviceType::New(MEM_MAJOR, 5);
const DeviceType DeviceType::FULL = DeviceType::New(MEM_MAJOR, 7);
const DeviceType DeviceType::RANDOM = DeviceType::New(MEM_MAJOR, 8);
const DeviceType DeviceType::URANDOM = DeviceType::New(MEM_MAJOR, 9);
const DeviceType DeviceType::KMSG = DeviceType::New(MEM_MAJOR, 11);

const DeviceType DeviceType::TTY = DeviceType::New(TTY_ALT_MAJOR, 0);
const DeviceType DeviceType::PTMX = DeviceType::New(TTY_ALT_MAJOR, 2);

const DeviceType DeviceType::HW_RANDOM = DeviceType::New(MISC_MAJOR, 183);
const DeviceType DeviceType::UINPUT = DeviceType::New(MISC_MAJOR, 223);
const DeviceType DeviceType::FUSE = DeviceType::New(MISC_MAJOR, 229);
const DeviceType DeviceType::DEVICE_MAPPER = DeviceType::New(MISC_MAJOR, 236);
const DeviceType DeviceType::LOOP_CONTROL = DeviceType::New(MISC_MAJOR, 237);

const DeviceType DeviceType::FB0 = DeviceType::New(FB_MAJOR, 0);
const DeviceType DeviceType::TUN = DeviceType::New(MISC_MAJOR, 200);

}  // namespace starnix_uapi
