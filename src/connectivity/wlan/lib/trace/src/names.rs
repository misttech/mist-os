// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ffi::CStr;

pub const CATEGORY_WLAN: &'static CStr = c"wlan";
pub const NAME_WLANCFG_START: &'static CStr = c"wlancfg:start";

// This name should be the same as defined in
// //src/connectivity/wlan/drivers/lib/log/cpp/include/common/wlan/drivers/log.h
pub const NAME_WLANSOFTMAC_TX: &'static CStr = c"wlansoftmac:tx";
