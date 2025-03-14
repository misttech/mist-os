
// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <memory>

#include <acpica/acpi.h>
#include <fbl/alloc_checker.h>
#include <kernel/thread.h>
#include <object/handle.h>
#include <object/interrupt_dispatcher.h>
#include <object/interrupt_event_dispatcher.h>

#include "zircon/system/ulib/acpica/oszircon.h"

// Wrapper structs for interfacing between our interrupt handler convention and
// ACPICA's
struct AcpiIrqThread {
  Thread* thread;
  ACPI_OSD_HANDLER handler;
  KernelHandle<InterruptDispatcher> irq_handle;
  UINT32 interrupt_number;
  void* context;
};

static int acpi_irq_thread(void* arg) {
  auto real_arg = static_cast<AcpiIrqThread*>(arg);
  while (1) {
    zx_time_t timestamp;
    zx_status_t status = real_arg->irq_handle.dispatcher()->WaitForInterrupt(&timestamp);
    if (status != ZX_OK) {
      break;
    }

    //  TODO: Should we do something with the return value from the handler?
    real_arg->handler(real_arg->context);
  }
  return 0;
}

static std::unique_ptr<AcpiIrqThread> sci_irq;

/**
 * @brief Install a handler for a hardware interrupt.
 *
 * @param InterruptLevel Interrupt level that the handler will service.
 * @param Handler Address of the handler.
 * @param Context A context value that is passed to the handler when the
 *        interrupt is dispatched.
 *
 * @return AE_OK The handler was successfully installed.
 * @return AE_BAD_PARAMETER The InterruptNumber is invalid or the Handler
 *         pointer is NULL.
 * @return AE_ALREADY_EXISTS A handler for this interrupt level is already
 *         installed.
 */
ACPI_STATUS AcpiOsInstallInterruptHandler(UINT32 InterruptLevel, ACPI_OSD_HANDLER Handler,
                                          void* Context) {
  // Note that InterruptLevel here is ISA IRQs (or global of the legacy PIC
  // doesn't exist), not system exceptions.

  // TODO: Clean this up to be less x86 centric.

  if (InterruptLevel == 0) {
    /* Some buggy firmware fails to populate the SCI_INT field of the FADT
     * properly.  0 is a known bad value, since the legacy PIT uses it and
     * cannot be remapped.  Just lie and say we installed a handler; this
     * system will just never receive an SCI.  If we return an error here,
     * ACPI init will fail completely, and the system will be unusable. */
    return AE_OK;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<AcpiIrqThread> arg(new (&ac) AcpiIrqThread());
  if (!ac.check()) {
    return AE_NO_MEMORY;
  }

  zx_rights_t rights;
  KernelHandle<InterruptDispatcher> handle;
  zx_status_t status =
      InterruptEventDispatcher::Create(&handle, &rights, InterruptLevel, ZX_INTERRUPT_REMAP_IRQ);
  if (status != ZX_OK) {
    return AE_ERROR;
  }
  arg->handler = Handler;
  arg->context = Context;
  arg->irq_handle = ktl::move(handle);
  // |InterruptLevel| in the spec appears to be the interrupt number based on
  // the errors returned in ACPICA 9.5.1, despite the name.
  arg->interrupt_number = InterruptLevel;

  arg->thread = Thread::Create("acpi_irq", acpi_irq_thread, arg.get(), DEFAULT_PRIORITY);
  if (arg->thread == nullptr) {
    return AE_ERROR;
  }

  sci_irq = std::move(arg);
  return AE_OK;
}

/**
 * @brief Remove an interrupt handler.
 *
 * @param InterruptNumber Interrupt number that the handler is currently
 *        servicing.
 * @param Handler Address of the handler that was previously installed.
 *
 * @return AE_OK The handler was successfully removed.
 * @return AE_BAD_PARAMETER The InterruptNumber is invalid, the Handler
 *         pointer is NULL, or the Handler address is no the same as the one
 *         currently installed.
 * @return AE_NOT_EXIST There is no handler installed for this interrupt level.
 */
ACPI_STATUS AcpiOsRemoveInterruptHandler(UINT32 InterruptNumber, ACPI_OSD_HANDLER Handler) {
  ZX_DEBUG_ASSERT(sci_irq != nullptr);
  ZX_DEBUG_ASSERT_MSG(sci_irq->interrupt_number == InterruptNumber, "%#x != %#x",
                      sci_irq->interrupt_number, InterruptNumber);
  sci_irq->irq_handle.dispatcher()->Destroy();
  sci_irq->thread->Join(nullptr, ZX_TIME_INFINITE);
  sci_irq.reset();
  return AE_OK;
}
