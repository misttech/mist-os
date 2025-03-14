// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2016, Google, Inc. All rights reserved
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_DEV_PCIE_INCLUDE_DEV_PCIE_IRQS_H_
#define ZIRCON_KERNEL_DEV_PCIE_INCLUDE_DEV_PCIE_IRQS_H_

#include <assert.h>
#include <sys/types.h>

#include <dev/interrupt.h>
#include <dev/pcie_platform.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/macros.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/spinlock.h>
#include <lockdep/lock_traits.h>
#include <region-alloc/region-alloc.h>

/* Fwd decls */
class PcieDevice;

// Trait for the legacy list of shared handles.
struct SharedHandlerTrait {
  using NodeState = fbl::DoublyLinkedListNodeState<PcieDevice*>;
  // This is defined after the layout of PcieDevice is visible.
  static NodeState& node_state(PcieDevice& device);
};
using DeviceHandlerList = fbl::DoublyLinkedListCustomTraits<PcieDevice*, SharedHandlerTrait>;

/**
 * Enumeration which defines the IRQ modes a PCIe device may be operating in.
 * IRQ modes are exclusive, a device may be operating in only one mode at any
 * given point in time.  Drivers may query the maximum number of IRQs supported
 * by each mode using the pcie_query_irq_mode_capabilities function.  Drivers
 * may request a particular number of IRQs be allocated when selecting an IRQ
 * mode with pcie_set_irq_mode.  IRQ identifiers used in the system when
 * registering, un-registering and dispatching IRQs are on the range [0, N-1]
 * where N are the number of IRQs successfully allocated using a call to
 * pcie_set_irq_mode.
 *
 * ++ PCIE_IRQ_MODE_DISABLED
 *    All IRQs are disabled.  0 total IRQs are supported in this mode.
 *
 * ++ PCIE_IRQ_MODE_LEGACY
 *    Devices may support up to 1 legacy IRQ in total.  Exclusive IRQ access
 *    cannot be guaranteed (the IRQ may be shared with other devices)
 *
 * ++ PCIE_IRQ_MODE_MSI
 *    Devices may support up to 32 MSI IRQs in total.  IRQs may be allocated
 *    exclusively, resources permitting.
 *
 * ++ PCIE_IRQ_MODE_MSI_X
 *    Devices may support up to 2048 MSI-X IRQs in total.  IRQs may be allocated
 *    exclusively, resources permitting.
 */
typedef enum pcie_irq_mode {
  PCIE_IRQ_MODE_DISABLED = 0,
  PCIE_IRQ_MODE_LEGACY = 1,
  PCIE_IRQ_MODE_LEGACY_NOACK = 2,
  PCIE_IRQ_MODE_MSI = 3,
  PCIE_IRQ_MODE_MSI_X = 4,
  PCIE_IRQ_MODE_COUNT = 5,
} pcie_irq_mode_t;

/**
 * A structure used to hold output parameters when calling
 * pcie_query_irq_mode_capabilities
 */
typedef struct pcie_irq_mode_caps {
  uint max_irqs; /** The maximum number of IRQ supported by the selected mode */
  /**
   * For MSI or MSI-X, indicates whether or not per-vector-masking has been
   * implemented by the hardware.
   */
  bool per_vector_masking_supported;
} pcie_irq_mode_caps_t;

/**
 * An enumeration of the permitted return values from a PCIe IRQ handler.
 *
 * ++ PCIE_IRQRET_NO_ACTION
 *    Do not mask the IRQ.
 *
 * ++ PCIE_IRQRET_MASK
 *    Mask the IRQ if (and only if) per vector masking is supported.
 */
typedef enum pcie_irq_handler_retval {
  PCIE_IRQRET_NO_ACTION = 0x0,
  PCIE_IRQRET_MASK = 0x1,
} pcie_irq_handler_retval_t;

/**
 * A structure used to hold the details about the currently configured IRQ mode
 * of a device.  Used in conjunction with pcie_get_irq_mode.
 */
typedef struct pcie_irq_mode_info {
  pcie_irq_mode_t mode;      /// The currently configured mode.
  uint max_handlers;         /// The max number of handlers for the mode.
  uint registered_handlers;  /// The current number of registered handlers.
} pcie_irq_mode_info_t;

/**
 * Definition of the callback registered with pcie_register_irq_handler.  This
 * callback will be called by a bus central IRQ dispatcher any time a chosen
 * device IRQ occurs.
 *
 * @note Masked/unmasked status of an IRQ MUST not be manipulated via the API
 * during an IRQ handler dispatch.  If an IRQ needs to be masked as part of a
 * handler's behavior, the appropriate return value should be used instead of in
 * the API.  @see pcie_irq_handler_retval_t
 *
 * @param dev A pointer to the pci device for which this IRQ occurred.
 * @param irq_id The 0-indexed ID of the IRQ which occurred.
 * @param ctx The context pointer registered when registering the handler.
 */
typedef pcie_irq_handler_retval_t (*pcie_irq_handler_fn_t)(const PcieDevice& dev, uint irq_id,
                                                           void* ctx);

/**
 * Structure used internally to hold the state of a registered handler.
 */
struct pcie_irq_handler_state_t {
  DECLARE_SPINLOCK(pcie_irq_handler_state_t) lock;
  pcie_irq_handler_fn_t handler = nullptr;
  void* ctx = nullptr;
  PcieDevice* dev = nullptr;
  uint pci_irq_id;
  bool masked;
};

/**
 * Class for managing shared legacy IRQ handlers.
 * TODO(johngro): Make this an inner class of PcieDevice
 */
class SharedLegacyIrqHandler
    : public fbl::SinglyLinkedListable<fbl::RefPtr<SharedLegacyIrqHandler>>,
      public fbl::RefCounted<SharedLegacyIrqHandler> {
 public:
  static fbl::RefPtr<SharedLegacyIrqHandler> Create(uint irq_id);
  ~SharedLegacyIrqHandler();

  void AddDevice(PcieDevice& dev);
  void RemoveDevice(PcieDevice& dev);

  uint irq_id() const { return irq_id_; }

  // Disallow copying, assigning and moving.
  DISALLOW_COPY_ASSIGN_AND_MOVE(SharedLegacyIrqHandler);

 private:
  explicit SharedLegacyIrqHandler(uint irq_id);

  static void HandlerThunk(void* arg) {
    DEBUG_ASSERT(arg);
    reinterpret_cast<SharedLegacyIrqHandler*>(arg)->Handler();
  }

  void Handler();

  DeviceHandlerList device_handler_list_;
  // TODO(https://fxbug.dev/42108122): Tracking of this lock is disabled to silence the circular
  // dependency warning from lockdep. The circular dependency is a known issue
  // that will be addressed by refactoring PCI support into userspace.
  mutable DECLARE_SPINLOCK(SharedLegacyIrqHandler,
                           lockdep::LockFlagsTrackingDisabled) device_handler_list_lock_;
  const uint irq_id_;
};

#endif  // ZIRCON_KERNEL_DEV_PCIE_INCLUDE_DEV_PCIE_IRQS_H_
