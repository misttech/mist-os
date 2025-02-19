// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>
#include <zircon/syscalls/hypervisor.h>

#include <fbl/ref_ptr.h>
#include <object/guest_dispatcher.h>
#include <object/handle.h>
#include <object/port_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/resource.h>
#include <object/vcpu_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>

zx_status_t sys_guest_create(zx_handle_t resource, uint32_t options, zx_handle_t* guest_handle,
                             zx_handle_t* vmar_handle) {
  zx_status_t status =
      validate_ranged_resource(resource, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_HYPERVISOR_BASE, 1);
  if (status != ZX_OK) {
    return status;
  }

  KernelHandle<GuestDispatcher> new_guest_handle;
  KernelHandle<VmAddressRegionDispatcher> new_vmar_handle;
  zx_rights_t guest_rights, vmar_rights;
  status = GuestDispatcher::Create(options, &new_guest_handle, &guest_rights, &new_vmar_handle,
                                   &vmar_rights);
  if (status != ZX_OK) {
    return status;
  }
  auto up = ProcessDispatcher::GetCurrent();
  status = up->MakeAndAddHandle(ktl::move(new_guest_handle), guest_rights, guest_handle);
  if (status != ZX_OK) {
    return status;
  }
  return up->MakeAndAddHandle(ktl::move(new_vmar_handle), vmar_rights, vmar_handle);
}

zx_status_t sys_guest_set_trap(zx_handle_t handle, uint32_t kind, zx_vaddr_t addr, size_t size,
                               zx_handle_t port_handle, uint64_t key) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<GuestDispatcher> guest;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &guest);
  if (status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<PortDispatcher> port;
  if (port_handle != ZX_HANDLE_INVALID) {
    status = up->handle_table().GetDispatcherWithRights(*up, port_handle, ZX_RIGHT_WRITE, &port);
    if (status != ZX_OK) {
      return status;
    }
  }

  return guest->SetTrap(kind, addr, size, ktl::move(port), key);
}

zx_status_t sys_vcpu_create(zx_handle_t guest_handle, uint32_t options, zx_vaddr_t entry,
                            zx_handle_t* out) {
  if (options != 0u) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<GuestDispatcher> guest;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, guest_handle, ZX_RIGHT_MANAGE_THREAD, &guest);
  if (status != ZX_OK) {
    return status;
  }

  KernelHandle<VcpuDispatcher> handle;
  zx_rights_t rights;
  status = VcpuDispatcher::Create(guest, entry, &handle, &rights);
  if (status != ZX_OK) {
    return status;
  }

  return up->MakeAndAddHandle(ktl::move(handle), rights, out);
}

zx_status_t sys_vcpu_enter(zx_handle_t handle, user_out_ptr<zx_port_packet_t> user_packet) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<VcpuDispatcher> vcpu;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_EXECUTE, &vcpu);
  if (status != ZX_OK) {
    return status;
  }

  zx_port_packet packet{};
  status = vcpu->Enter(packet);
  if (status != ZX_OK) {
    return status;
  }

  status = user_packet.copy_to_user(packet);
  if (status != ZX_OK) {
    return status;
  }

  return ZX_OK;
}

zx_status_t sys_vcpu_kick(zx_handle_t handle) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<VcpuDispatcher> vcpu;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_EXECUTE, &vcpu);
  if (status != ZX_OK) {
    return status;
  }

  vcpu->Kick();
  return ZX_OK;
}

zx_status_t sys_vcpu_interrupt(zx_handle_t handle, uint32_t vector) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<VcpuDispatcher> vcpu;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_SIGNAL, &vcpu);
  if (status != ZX_OK) {
    return status;
  }

  return vcpu->Interrupt(vector);
}

zx_status_t sys_vcpu_read_state(zx_handle_t handle, uint32_t kind, user_out_ptr<void> user_buffer,
                                size_t buffer_size) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<VcpuDispatcher> vcpu;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_READ, &vcpu);
  if (status != ZX_OK) {
    return status;
  }
  if (kind != ZX_VCPU_STATE || buffer_size != sizeof(zx_vcpu_state_t)) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx_vcpu_state_t state{};
  status = vcpu->ReadState(state);
  if (status != ZX_OK) {
    return status;
  }

  return user_buffer.reinterpret<zx_vcpu_state_t>().copy_to_user(state);
}

template <typename T>
static zx_status_t WriteState(VcpuDispatcher* vcpu, user_in_ptr<const void> user_buffer,
                              size_t buffer_size) {
  if (buffer_size != sizeof(T)) {
    return ZX_ERR_INVALID_ARGS;
  }

  T state{};
  zx_status_t status = user_buffer.reinterpret<const T>().copy_from_user(&state);
  if (status != ZX_OK) {
    return status;
  }
  return vcpu->WriteState(state);
}

zx_status_t sys_vcpu_write_state(zx_handle_t handle, uint32_t kind,
                                 user_in_ptr<const void> user_buffer, size_t buffer_size) {
  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<VcpuDispatcher> vcpu;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &vcpu);
  if (status != ZX_OK) {
    return status;
  }

  switch (kind) {
    case ZX_VCPU_STATE:
      return WriteState<zx_vcpu_state_t>(vcpu.get(), user_buffer, buffer_size);
    case ZX_VCPU_IO:
      return WriteState<zx_vcpu_io_t>(vcpu.get(), user_buffer, buffer_size);
    default:
      return ZX_ERR_INVALID_ARGS;
  }
}
