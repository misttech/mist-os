// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_RECOVERY_FACTORY_RESET_FACTORY_RESET_H_
#define SRC_RECOVERY_FACTORY_RESET_FACTORY_RESET_H_

#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fuchsia/hardware/power/statecontrol/cpp/fidl.h>
#include <fuchsia/recovery/cpp/fidl.h>
#include <zircon/types.h>

#include <fbl/unique_fd.h>

namespace factory_reset {

// Implements a simple version of Factory Reset that shreds zxcrypt and then
// reboots.
class FactoryReset : public fuchsia::recovery::FactoryReset {
 public:
  FactoryReset(fbl::unique_fd dev_fd, fuchsia::hardware::power::statecontrol::AdminPtr admin,
               fidl::ClientEnd<fuchsia_fshost::Admin> fshost_admin);
  // Performs the factory reset.
  void Reset(ResetCallback callback) override;

 private:
  // Finds the zxcrypt partition, then overwrites its superblocks with random
  // data, causing them to be unusable.
  zx_status_t Shred() const;

  fuchsia::hardware::power::statecontrol::AdminPtr admin_;
  fbl::unique_fd dev_fd_;
  fidl::ClientEnd<fuchsia_fshost::Admin> fshost_admin_;
};

}  // namespace factory_reset

#endif  // SRC_RECOVERY_FACTORY_RESET_FACTORY_RESET_H_
