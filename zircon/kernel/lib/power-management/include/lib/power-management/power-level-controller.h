// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_LEVEL_CONTROLLER_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_LEVEL_CONTROLLER_H_

#include <lib/stdcompat/array.h>
#include <lib/zx/result.h>
#include <zircon/syscalls-next.h>

#include <atomic>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>

namespace power_management {

// Forward declaration.
struct PowerLevelUpdateRequest;

// Interface representing an entity in charge of update requests that are not handled by the kernel.
//
// In essence there will be only one type of transition handler, but we introduce the interface to
// decouple most of the code from the kernel environment.
class PowerLevelController : public fbl::RefCounted<PowerLevelController> {
 public:
  virtual ~PowerLevelController() = default;

  // Posts a pending request making it available for the entity in charge of executing the
  // transition. This method must be complemented by an acknowledgment of a transition.
  virtual zx::result<> Post(const PowerLevelUpdateRequest& pending) = 0;

  // Unique id of the `PowerLevelController`, used for validation.
  virtual uint64_t id() const = 0;

  // Determines whether the controller is serving requests or not.
  // Allows scheduler instances to interrogate whether they should consider any
  // active power levels or not.
  bool is_serving() const { return serving_.load(std::memory_order_relaxed); }

 protected:
  // Implementations will determine when to switch this flag. The flag must always
  // be initialized as true.
  std::atomic<bool> serving_{true};
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_POWER_LEVEL_CONTROLLER_H_
