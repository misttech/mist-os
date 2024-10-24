// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_INTERRUPTIBLE_EVENT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_INTERRUPTIBLE_EVENT_H_

#include <fbl/ref_counted_upgradeable.h>

namespace starnix_sync {

class InterruptibleEvent : public fbl::RefCountedUpgradeable<InterruptibleEvent> {};

}  // namespace starnix_sync

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_INTERRUPTIBLE_EVENT_H_
