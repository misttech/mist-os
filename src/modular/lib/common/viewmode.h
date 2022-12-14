// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MODULAR_LIB_COMMON_VIEWMODE_H_
#define SRC_MODULAR_LIB_COMMON_VIEWMODE_H_

namespace modular {

// Indicates the view API to use, if any.
enum class ViewMode {
  HEADLESS = 1,

  FLATLAND = 2,

  GFX = 3,
};

}  // namespace modular

#endif  // SRC_MODULAR_LIB_COMMON_VIEWMODE_H_
