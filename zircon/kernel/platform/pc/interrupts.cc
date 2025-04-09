// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2009 Corey Tabaka
// Copyright (c) 2015 Intel Corporation
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <debug.h>
#include <lib/acpi_lite/apic.h>
#include <lib/acpi_lite/structures.h>
#include <lib/ktrace.h>
#include <sys/types.h>
#include <trace.h>
#include <zircon/types.h>

#include <arch/regs.h>
#include <arch/x86.h>
#include <arch/x86/feature.h>
#include <arch/x86/platform_access.h>
#include <arch/x86/pv.h>
#include <dev/interrupt.h>
#include <fbl/algorithm.h>
#include <kernel/stats.h>
#include <kernel/thread.h>
#include <ktl/optional.h>
#include <lk/init.h>
#include <object/resource_dispatcher.h>
#include <platform/pc.h>
#include <platform/pc/acpi.h>
#include <platform/pic.h>

#include "interrupt_manager.h"
#include "platform_p.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

#define IFRAME_PC(frame) ((frame)->ip)

namespace {

// Values from ioapic to cache for calls to interrupt_get_base_vector / interrupt_get_max_vector
ktl::optional<GsiRange> x64_gsis;

// Interface passed to InterruptManager to construct the real system interrupt
// manager.
class IoApic {
 public:
  static bool IsValidInterrupt(unsigned int vector, uint32_t flags) {
    return is_valid_interrupt(vector, flags);
  }
  static uint8_t FetchIrqVector(unsigned int vector) { return apic_io_fetch_irq_vector(vector); }
  static void ConfigureIrqVector(uint32_t global_irq, uint8_t x86_vector) {
    apic_io_configure_irq_vector(global_irq, x86_vector);
  }
  static void ConfigureIrq(uint32_t global_irq, enum interrupt_trigger_mode trig_mode,
                           enum interrupt_polarity polarity,
                           enum apic_interrupt_delivery_mode del_mode, bool mask,
                           enum apic_interrupt_dst_mode dst_mode, uint8_t dst, uint8_t vector) {
    apic_io_configure_irq(global_irq, trig_mode, polarity, del_mode, mask, dst_mode, dst, vector);
  }
  static void MaskIrq(uint32_t global_irq, bool mask) { apic_io_mask_irq(global_irq, mask); }
  static zx_status_t FetchIrqConfig(uint32_t global_irq, enum interrupt_trigger_mode* trig_mode,
                                    enum interrupt_polarity* polarity) {
    return apic_io_fetch_irq_config(global_irq, trig_mode, polarity);
  }
};

// Singleton for managing interrupts.  This is fully initialized in
// platform_init_apic().
InterruptManager<IoApic> kInterruptManager;

// Convert an ACPI entry into the format required by the platform's APIC code.
io_apic_isa_override ParseIsaOverride(const acpi_lite::AcpiMadtIntSourceOverrideEntry* record) {
  // 0 means ISA, ISOs are only ever for ISA IRQs
  if (record->bus != 0) {
    panic("Invalid bus for IO APIC interrupt override.\n");
  }

  // "Conforms" below means conforms to the bus spec: edge triggered and active high.
  const uint32_t flags = record->flags;
  const interrupt_polarity polarity = [flags]() {
    uint32_t polarity = flags & ACPI_MADT_FLAG_POLARITY_MASK;
    switch (polarity) {
      case ACPI_MADT_FLAG_POLARITY_CONFORMS:
      case ACPI_MADT_FLAG_POLARITY_HIGH:
        return IRQ_POLARITY_ACTIVE_HIGH;
      case ACPI_MADT_FLAG_POLARITY_LOW:
        return IRQ_POLARITY_ACTIVE_LOW;
      default:
        panic("Unknown IRQ polarity in override: %u\n", polarity);
    }
  }();

  const interrupt_trigger_mode trigger_mode = [flags]() {
    uint32_t trigger = flags & ACPI_MADT_FLAG_TRIGGER_MASK;
    switch (trigger) {
      case ACPI_MADT_FLAG_TRIGGER_CONFORMS:
      case ACPI_MADT_FLAG_TRIGGER_EDGE:
        return IRQ_TRIGGER_MODE_EDGE;
        break;
      case ACPI_MADT_FLAG_TRIGGER_LEVEL:
        return IRQ_TRIGGER_MODE_LEVEL;
      default:
        panic("Unknown IRQ trigger in override: %u\n", trigger);
    }
  }();

  return io_apic_isa_override{
      .isa_irq = record->source,
      .remapped = true,
      .tm = trigger_mode,
      .pol = polarity,
      .global_irq = record->global_sys_interrupt,
  };
}

}  // namespace

static void platform_init_apic(uint level) {
  pic_map(PIC1_BASE, PIC2_BASE);
  pic_disable();

  acpi_lite::AcpiParser& parser = GlobalAcpiLiteParser();

  // Enumerate the IO APICs
  fbl::Vector<io_apic_descriptor> descriptors;
  zx_status_t status = acpi_lite::EnumerateIoApics(
      parser, [&descriptors](const acpi_lite::AcpiMadtIoApicEntry& descriptor) {
        fbl::AllocChecker ac;
        descriptors.push_back(
            io_apic_descriptor{
                .apic_id = descriptor.io_apic_id,
                .global_irq_base = descriptor.global_system_interrupt_base,
                .paddr = descriptor.io_apic_address,
            },
            &ac);
        return ac.check() ? ZX_OK : ZX_ERR_NO_MEMORY;
      });
  if (status != ZX_OK) {
    panic("Could not get IO APIC details: %d\n", status);
  }

  // Enumerate interrupt source overrides.
  fbl::Vector<io_apic_isa_override> overrides;
  status = acpi_lite::EnumerateIoApicIsaOverrides(
      parser, [&overrides](const acpi_lite::AcpiMadtIntSourceOverrideEntry& isa_override) {
        fbl::AllocChecker ac;
        overrides.push_back(ParseIsaOverride(&isa_override), &ac);
        return ac.check() ? ZX_OK : ZX_ERR_NO_MEMORY;
      });
  if (status != ZX_OK) {
    panic("Could not get interrupt source overrides: %d\n", status);
  }

  apic_vm_init();
  apic_local_init();
  apic_io_init(descriptors.data(), descriptors.size(), overrides.data(), overrides.size());
  x64_gsis = apic_io_get_gsi_range();

  ASSERT(arch_ints_disabled());

  // Initialize the delivery modes/targets for the ISA interrupts
  uint8_t bsp_apic_id = apic_bsp_id();
  for (uint8_t irq = 0; irq < 8; ++irq) {
    // Explicitly skip mapping the PIC2 interrupt, since it is actually
    // just used internally on the PICs for daisy chaining.  QEMU remaps
    // ISA IRQ 0 to global IRQ 2, but does not remap ISA IRQ 2 off of
    // global IRQ 2, so skipping this mapping also prevents a collision
    // with the PIT IRQ.
    if (irq != ISA_IRQ_PIC2) {
      apic_io_configure_isa_irq(irq, DELIVERY_MODE_FIXED, IO_APIC_IRQ_MASK, DST_MODE_PHYSICAL,
                                bsp_apic_id, 0);
    }
    apic_io_configure_isa_irq(static_cast<uint8_t>(irq + 8), DELIVERY_MODE_FIXED, IO_APIC_IRQ_MASK,
                              DST_MODE_PHYSICAL, bsp_apic_id, 0);
  }

  status = kInterruptManager.Init();
  ResourceDispatcher::InitializeAllocator(ZX_RSRC_KIND_IRQ, interrupt_get_base_vector(),
                                          interrupt_get_max_vector());
  ASSERT(status == ZX_OK);
}
LK_INIT_HOOK(apic, &platform_init_apic, LK_INIT_LEVEL_VM + 2)

zx_status_t mask_interrupt(unsigned int vector) { return kInterruptManager.MaskInterrupt(vector); }

zx_status_t unmask_interrupt(unsigned int vector) {
  return kInterruptManager.UnmaskInterrupt(vector);
}

zx_status_t configure_interrupt(unsigned int vector, enum interrupt_trigger_mode tm,
                                enum interrupt_polarity pol) {
  return kInterruptManager.ConfigureInterrupt(vector, tm, pol);
}

zx_status_t get_interrupt_config(unsigned int vector, enum interrupt_trigger_mode* tm,
                                 enum interrupt_polarity* pol) {
  return kInterruptManager.GetInterruptConfig(vector, tm, pol);
}

void platform_irq(iframe_t* frame) {
  CPU_STATS_INC(interrupts);
  // get the current vector
  uint64_t x86_vector = frame->vector;
  DEBUG_ASSERT(x86_vector >= X86_INT_PLATFORM_BASE && x86_vector <= X86_INT_PLATFORM_MAX);

  ktrace::Scope trace = KTRACE_CPU_BEGIN_SCOPE("kernel:irq", "irq", ("irq #", x86_vector));

  LTRACEF_LEVEL(2, "cpu %u currthread %p vector %lu pc %#" PRIxPTR "\n", arch_curr_cpu_num(),
                Thread::Current::Get(), x86_vector, (uintptr_t)IFRAME_PC(frame));

  // deliver the interrupt
  kInterruptManager.InvokeX86Vector(static_cast<uint8_t>(x86_vector));

  // NOTE: On x86, we always deactivate the interrupt.
  apic_issue_eoi();

  LTRACEF_LEVEL(2, "cpu %u exit\n", arch_curr_cpu_num());
}

zx_status_t register_int_handler(unsigned int vector, int_handler handler, void* arg) {
  return kInterruptManager.RegisterInterruptHandler(vector, handler, arg);
}

zx_status_t register_permanent_int_handler(unsigned int vector, int_handler handler, void* arg) {
  return kInterruptManager.RegisterInterruptHandler(vector, handler, arg, true /* Permanent */);
}

// On x64 these methods return the base and max Global System Interrupts as
// defined by ACPI and described by MADT tables.
// ACPI Spec 6.1, section 5.2.12 & section 5.2.13.
uint32_t interrupt_get_base_vector(void) {
  ZX_DEBUG_ASSERT(x64_gsis.has_value());
  return x64_gsis.value().start;
}
uint32_t interrupt_get_max_vector(void) {
  ZX_DEBUG_ASSERT(x64_gsis.has_value());
  return x64_gsis.value().end;
}

bool is_valid_interrupt(unsigned int vector, uint32_t flags) {
  return apic_io_is_valid_irq(vector);
}

unsigned int remap_interrupt(unsigned int vector) {
  if (vector >= NUM_ISA_IRQS) {
    return vector;
  }
  return apic_io_isa_to_global(static_cast<uint8_t>(vector));
}

void shutdown_interrupts(void) { pic_disable(); }

void shutdown_interrupts_curr_cpu(void) {
  if (x86_hypervisor_has_pv_eoi()) {
    MsrAccess msr;
    PvEoi::get()->Disable(&msr);
  }

  // TODO(maniscalco): Walk interrupt redirection entries and make sure nothing targets this CPU.
}

// Intel 64 socs support the IOAPIC and Local APIC which support MSI by default.
// See 10.1, 10.4, and 10.11 of Intel® 64 and IA-32 Architectures Software Developer’s
// Manual 3A
bool msi_is_supported(void) { return true; }
bool msi_supports_masking() { return false; }
// Since we do not support masking on x64 it is an error to call |msi_mask_unmask|.
void msi_mask_unmask(const msi_block_t* block, uint msi_id, bool mask) { PANIC_UNIMPLEMENTED; }

zx_status_t msi_alloc_block(uint requested_irqs, bool can_target_64bit, bool is_msix,
                            msi_block_t* out_block) {
  return kInterruptManager.MsiAllocBlock(requested_irqs, can_target_64bit, is_msix, out_block);
}

void msi_free_block(msi_block_t* block) { return kInterruptManager.MsiFreeBlock(block); }

void msi_register_handler(const msi_block_t* block, uint msi_id, int_handler handler, void* ctx) {
  return kInterruptManager.MsiRegisterHandler(block, msi_id, handler, ctx);
}
