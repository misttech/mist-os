// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_HYPERVISOR_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_HYPERVISOR_H_

#include <lib/zx/result.h>
#include <zircon/syscalls/hypervisor.h>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

typedef struct zx_port_packet zx_port_packet_t;
class PortDispatcher;
class VmAddressRegion;

// Represents a guest within the hypervisor.
class Guest {
 public:
  static zx::result<ktl::unique_ptr<Guest>> Create();
  ~Guest();

  Guest(Guest&&) = delete;
  Guest& operator=(Guest&&) = delete;
  Guest(const Guest&) = delete;
  Guest& operator=(const Guest&) = delete;

  zx::result<> SetTrap(uint32_t kind, zx_vaddr_t addr, size_t len, fbl::RefPtr<PortDispatcher> port,
                       uint64_t key);

  fbl::RefPtr<VmAddressRegion> RootVmar() const;
};

using NormalGuest = Guest;

// Represents a virtual CPU within a guest.
class Vcpu {
 public:
  static zx::result<ktl::unique_ptr<Vcpu>> Create(Guest& guest, zx_vaddr_t entry);
  ~Vcpu();

  Vcpu(Vcpu&&) = delete;
  Vcpu& operator=(Vcpu&&) = delete;
  Vcpu(const Vcpu&) = delete;
  Vcpu& operator=(const Vcpu&) = delete;

  zx::result<> Enter(zx_port_packet_t& packet);
  void Kick();
  void Interrupt(uint32_t vector);
  zx::result<> ReadState(zx_vcpu_state_t& state) const;
  zx::result<> WriteState(const zx_vcpu_state_t& state);
  zx::result<> WriteState(const zx_vcpu_io_t& io_state) { return zx::error(ZX_ERR_INVALID_ARGS); }

  void GetInfo(zx_info_vcpu_t* info);
};

using NormalVcpu = Vcpu;

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_HYPERVISOR_H_
