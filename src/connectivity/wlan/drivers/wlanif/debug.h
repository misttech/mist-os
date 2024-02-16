// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEBUG_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEBUG_H_

// ltrace_first and ltrace_rest are helper macros
#define ltrace_first(arg, ...) arg
#define ltrace_rest(arg, ...) __VA_OPT__(, ) __VA_ARGS__
#define ltrace_fn(logger, ...)                                                  \
  FDF_LOGL(ERROR, logger, "(%s)" __VA_OPT__(" ") ltrace_first(__VA_ARGS__, ""), \
           __func__ ltrace_rest(__VA_ARGS__))

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEBUG_H_
