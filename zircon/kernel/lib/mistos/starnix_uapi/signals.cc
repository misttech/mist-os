// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_uapi/signals.h"

namespace starnix_uapi {

bool Signal::is_unblockable() const { return UNBLOCKABLE_SIGNALS.has_signal(*this); }

}  // namespace starnix_uapi
