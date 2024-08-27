// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/ktrace.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <platform.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <arch/arch_ops.h>
#include <fbl/auto_lock.h>
#include <fbl/inline_array.h>
#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <object/handle.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/resource_dispatcher.h>
#include <object/suspend_token_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace {

KCOUNTER(thread_legacy_yield, "thread.legacy_yield")

// constexpr size_t kMaxDebugReadBlock = 64 * 1024u * 1024u;
// constexpr size_t kMaxDebugWriteBlock = 64 * 1024u * 1024u;

// TODO(https://fxbug.dev/42105890): copy_user_string may truncate the incoming string,
// and may copy extra data past the NUL.
// TODO(dbort): If anyone else needs this, move it into user_ptr.
zx_status_t copy_user_string(const char* src, size_t src_len, char* buf, size_t buf_len,
                             ktl::string_view* sp) {
  // Disallow 0 buf_len (since we are copying into it), but allow 0 src_len (to allow
  // "", src_len doesn't include '\0'). With the check for buf_len, we won't underflow
  // src_len below. Also, 0 src_len is valid input (for "" src strings).
  if (!src || buf_len == 0 || src_len > buf_len) {
    return ZX_ERR_INVALID_ARGS;
  }
  memcpy(buf, src, src_len);

  // ensure zero termination
  size_t str_len = (src_len == buf_len ? src_len - 1 : src_len);
  buf[str_len] = 0;
  *sp = ktl::string_view(buf);

  return ZX_OK;
}

}  // namespace

zx_status_t zx_thread_create(zx_handle_t process_handle, const char* _name, size_t name_len,
                             uint32_t options, zx_handle_t* out) {
  LTRACEF("process handle %x, options %#x\n", process_handle, options);

  // currently, the only valid option value is 0
  if (options != 0)
    return ZX_ERR_INVALID_ARGS;

  // copy out the name
  char buf[ZX_MAX_NAME_LEN];
  ktl::string_view sp;
  // Silently truncate the given name.
  if (name_len > sizeof(buf))
    name_len = sizeof(buf);
  zx_status_t result = copy_user_string(_name, name_len, buf, sizeof(buf), &sp);
  if (result != ZX_OK)
    return result;
  LTRACEF("name %s\n", buf);

  // convert process handle to process dispatcher
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  fbl::RefPtr<ProcessDispatcher> process;
  result = handle_table(up).GetDispatcherWithRights(*up, process_handle, ZX_RIGHT_MANAGE_THREAD,
                                                    &process);
  if (result != ZX_OK) {
    return result;
  }

  // create the thread dispatcher
  KernelHandle<ThreadDispatcher> handle;
  zx_rights_t thread_rights;
  result = ThreadDispatcher::Create(ktl::move(process), options, sp, &handle, &thread_rights);
  if (result != ZX_OK)
    return result;

  result = handle.dispatcher()->Initialize();
  if (result != ZX_OK) {
    return result;
  }

  return MakeAndAddHandle(ktl::move(handle), thread_rights, out);
}

zx_status_t zx_thread_start(zx_handle_t handle, zx_vaddr_t thread_entry, zx_vaddr_t stack,
                            uintptr_t arg1, uintptr_t arg2) {
  LTRACEF("handle %x, entry %#" PRIxPTR ", sp %#" PRIxPTR ", arg1 %#" PRIxPTR ", arg2 %#" PRIxPTR
          "\n",
          handle, thread_entry, stack, arg1, arg2);
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  fbl::RefPtr<ThreadDispatcher> thread;
  zx_status_t status =
      handle_table(up).GetDispatcherWithRights(*up, handle, ZX_RIGHT_MANAGE_THREAD, &thread);
  if (status != ZX_OK) {
    return status;
  }

  return thread->Start(ThreadDispatcher::EntryState{thread_entry, stack, arg1, arg2},
                       /* ensure_initial_thread= */ false);
}

void zx_thread_exit() {
  LTRACE_ENTRY;
  ThreadDispatcher::ExitCurrent();
}

#if 0
// zx_status_t zx_thread_read_state
zx_status_t sys_thread_read_state(zx_handle_t handle, uint32_t kind, user_out_ptr<void> buffer,
                                  size_t buffer_size) {
  LTRACEF("handle %x, kind %u\n", handle, kind);

  auto up = ProcessDispatcher::GetCurrent();

  // TODO(https://fxbug.dev/42105831): debug rights
  fbl::RefPtr<ThreadDispatcher> thread;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_READ, &thread);
  if (status != ZX_OK)
    return status;

  return thread->ReadState(static_cast<zx_thread_state_topic_t>(kind), buffer, buffer_size);
}

// zx_status_t zx_thread_write_state
zx_status_t sys_thread_write_state(zx_handle_t handle, uint32_t kind,
                                   user_in_ptr<const void> buffer, size_t buffer_size) {
  LTRACEF("handle %x, kind %u\n", handle, kind);

  if ((kind & ZX_THREAD_STATE_DEBUG_REGS) && !gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto up = ProcessDispatcher::GetCurrent();

  // TODO(https://fxbug.dev/42105831): debug rights
  fbl::RefPtr<ThreadDispatcher> thread;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &thread);
  if (status != ZX_OK)
    return status;

  return thread->WriteState(static_cast<zx_thread_state_topic_t>(kind), buffer, buffer_size);
}

// zx_status_t zx_thread_raise_exception
zx_status_t sys_thread_raise_exception(uint32_t options, zx_excp_type_t type,
                                       user_in_ptr<const zx_exception_context_t> _user_context) {
  if (options != ZX_EXCEPTION_TARGET_JOB_DEBUGGER || type != ZX_EXCP_USER || !_user_context) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx_exception_context_t user_context = {};
  zx_status_t status = _user_context.copy_from_user(&user_context);
  if (status != ZX_OK) {
    return status;
  }

  // TODO(https://fxbug.dev/42077468): Fill in the user registers once we have access.
  arch_exception_context_t context = {};
  context.user_synth_code = user_context.synth_code;
  context.user_synth_data = user_context.synth_data;

  auto thread = ThreadDispatcher::GetCurrent();
  thread->process()->OnUserExceptionForJobDebugger(thread, &context);
  return ZX_OK;
}
#endif

zx_status_t zx_thread_legacy_yield(uint32_t options) {
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }
  kcounter_add(thread_legacy_yield, 1);
  Thread::Current::Yield();
  return ZX_OK;
}

#if 0
// zx_status_t zx_task_suspend
zx_status_t sys_task_suspend(zx_handle_t handle, zx_handle_t* token) {
  LTRACE_ENTRY;

  auto up = ProcessDispatcher::GetCurrent();

  // TODO(https://fxbug.dev/42105711): Add support for jobs
  fbl::RefPtr<Dispatcher> task;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &task);
  if (status != ZX_OK)
    return status;

  KernelHandle<SuspendTokenDispatcher> new_token;
  zx_rights_t rights;
  status = SuspendTokenDispatcher::Create(ktl::move(task), &new_token, &rights);

  if (status == ZX_OK)
    status = up->MakeAndAddHandle(ktl::move(new_token), rights, token);

  return status;
}

// zx_status_t zx_task_suspend_token
zx_status_t sys_task_suspend_token(zx_handle_t handle, zx_handle_t* token) {
  return sys_task_suspend(handle, token);
}
#endif

zx_status_t zx_process_create(zx_handle_t job_handle, const char* _name, size_t name_len,
                              uint32_t options, zx_handle_t* proc_handle,
                              zx_handle_t* vmar_handle) {
  LTRACEF("job handle %x, options %#x\n", job_handle, options);

  // currently, the only valid option values are 0 or ZX_PROCESS_SHARED
  if (options != ZX_PROCESS_SHARED && options != 0)
    return ZX_ERR_INVALID_ARGS;

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // copy out the name
  char buf[ZX_MAX_NAME_LEN];
  ktl::string_view sp;
  // Silently truncate the given name.
  if (name_len > sizeof(buf))
    name_len = sizeof(buf);
  zx_status_t result = copy_user_string(_name, name_len, buf, sizeof(buf), &sp);
  if (result != ZX_OK)
    return result;
  LTRACEF("name %s\n", buf);

  fbl::RefPtr<JobDispatcher> job;
  auto status =
      handle_table(up).GetDispatcherWithRights(*up, job_handle, ZX_RIGHT_MANAGE_PROCESS, &job);
  if (status != ZX_OK) {
    return status;
  }

  // create a new process dispatcher
  KernelHandle<ProcessDispatcher> new_process_handle;
  KernelHandle<VmAddressRegionDispatcher> new_vmar_handle;
  zx_rights_t proc_rights, vmar_rights;
  result = ProcessDispatcher::Create(ktl::move(job), sp, options, &new_process_handle, &proc_rights,
                                     &new_vmar_handle, &vmar_rights);
  if (result != ZX_OK)
    return result;

  KTRACE_KERNEL_OBJECT("kernel:meta", new_process_handle.dispatcher()->get_koid(),
                       ZX_OBJ_TYPE_PROCESS, buf);

  result = MakeAndAddHandle(ktl::move(new_process_handle), proc_rights, proc_handle);
  if (result == ZX_OK)
    result = MakeAndAddHandle(ktl::move(new_vmar_handle), vmar_rights, vmar_handle);
  return result;
}

#if 0
// zx_status_t zx_process_create_shared
zx_status_t sys_process_create_shared(zx_handle_t shared_proc_handle, uint32_t options,
                                      user_in_ptr<const char> _name, size_t name_len,
                                      zx_handle_t* proc_handle,
                                      zx_handle_t* restricted_vmar_handle) {
  // currently, the only valid option value is 0
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();

  // We check the policy against the process calling zx_process_create, which
  // is the operative policy.
  // TODO(https://fxbug.dev/42181309): Figure out which policy check makes sense here.
  zx_status_t result = up->EnforceBasicPolicy(ZX_POL_NEW_PROCESS);
  if (result != ZX_OK) {
    return result;
  }

  // copy out the name
  char buf[ZX_MAX_NAME_LEN];
  ktl::string_view sp;
  // Silently truncate the given name.
  if (name_len > sizeof(buf)) {
    name_len = sizeof(buf);
  }
  result = copy_user_string(_name, name_len, buf, sizeof(buf), &sp);
  if (result != ZX_OK) {
    return result;
  }
  LTRACEF("name %s\n", buf);

  fbl::RefPtr<ProcessDispatcher> shared_proc;
  result = up->handle_table().GetDispatcherWithRights(*up, shared_proc_handle,
                                                      ZX_RIGHT_MANAGE_PROCESS, &shared_proc);
  if (result != ZX_OK) {
    return result;
  }

  // create a new process dispatcher
  KernelHandle<ProcessDispatcher> new_process_handle;
  KernelHandle<VmAddressRegionDispatcher> new_restricted_vmar_handle;
  zx_rights_t proc_rights, restricted_vmar_rights;
  result =
      ProcessDispatcher::CreateShared(shared_proc, sp, options, &new_process_handle, &proc_rights,
                                      &new_restricted_vmar_handle, &restricted_vmar_rights);
  if (result != ZX_OK) {
    return result;
  }

  KTRACE_KERNEL_OBJECT("kernel:meta", new_process_handle.dispatcher()->get_koid(),
                       ZX_OBJ_TYPE_PROCESS, buf);

  result = up->MakeAndAddHandle(ktl::move(new_process_handle), proc_rights, proc_handle);

  if (result == ZX_OK) {
    result = up->MakeAndAddHandle(ktl::move(new_restricted_vmar_handle), restricted_vmar_rights,
                                  restricted_vmar_handle);
  }

  return result;
}
#endif

// Note: This is used to start the main thread (as opposed to using
// sys_thread_start for that) for a few reasons:
// - less easily exploitable
//   We want to make sure we can't generically transfer handles to a process.
//   This has the nice property of restricting the evil (transferring handle
//   to new process) to exactly one spot, and can be called exactly once per
//   process, since it also pushes it into a new state.
// - maintains the state machine invariant that 'started' processes have one
//   thread running

zx_status_t zx_process_start(zx_handle_t process_handle, zx_handle_t thread_handle, zx_vaddr_t pc,
                             zx_vaddr_t sp, zx_handle_t arg_handle_value, uintptr_t arg2) {
  LTRACEF("phandle %x, thandle %x, pc %#" PRIxPTR ", sp %#" PRIxPTR
          ", arg_handle %x, arg2 %#" PRIxPTR "\n",
          process_handle, thread_handle, pc, sp, arg_handle_value, arg2);
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // get process dispatcher
  fbl::RefPtr<ProcessDispatcher> process;
  zx_status_t status =
      handle_table(up).GetDispatcherWithRights(*up, process_handle, ZX_RIGHT_WRITE, &process);
  if (status != ZX_OK) {
    if (arg_handle_value != ZX_HANDLE_INVALID) {
      handle_table(up).RemoveHandle(*up, arg_handle_value);
    }
    return status;
  }

  // get thread_dispatcher
  fbl::RefPtr<ThreadDispatcher> thread;
  status = handle_table(up).GetDispatcherWithRights(*up, thread_handle, ZX_RIGHT_WRITE, &thread);
  if (status != ZX_OK) {
    if (arg_handle_value != ZX_HANDLE_INVALID) {
      handle_table(up).RemoveHandle(*up, arg_handle_value);
    }
    return status;
  }

  HandleOwner arg_handle;
  if (arg_handle_value != ZX_HANDLE_INVALID) {
    arg_handle = handle_table(up).RemoveHandle(*up, arg_handle_value);
  }

  // test that the thread belongs to the starting process
  if (thread->process() != process.get())
    return ZX_ERR_ACCESS_DENIED;

  zx_handle_t arg_nhv = ZX_HANDLE_INVALID;
  if (arg_handle) {
    if (!arg_handle->HasRights(ZX_RIGHT_TRANSFER))
      return ZX_ERR_ACCESS_DENIED;
    arg_nhv = process->handle_table().MapHandleToValue(arg_handle);
    process->handle_table().AddHandle(ktl::move(arg_handle));
  }

  status =
      thread->Start(ThreadDispatcher::EntryState{pc, sp, static_cast<uintptr_t>(arg_nhv), arg2},
                    /* ensure_initial_thread */ true);
  if (status != ZX_OK) {
    if (arg_nhv != ZX_HANDLE_INVALID) {
      // Remove |arg_handle| from the process that failed to start.
      process->handle_table().RemoveHandle(*process, arg_nhv);
    }
    return status;
  }

  return ZX_OK;
}

void sys_process_exit(int64_t retcode) {
  LTRACEF("retcode %" PRId64 "\n", retcode);
  ProcessDispatcher::ExitCurrent(retcode);
}

#if 0
// zx_status_t zx_process_read_memory
zx_status_t sys_process_read_memory(zx_handle_t handle, zx_vaddr_t vaddr, user_out_ptr<void> buffer,
                                    size_t buffer_size, user_out_ptr<size_t> actual) {
  LTRACEF("vaddr 0x%" PRIxPTR ", size %zu\n", vaddr, buffer_size);

  if (!buffer)
    return ZX_ERR_INVALID_ARGS;
  if (buffer_size == 0 || buffer_size > kMaxDebugReadBlock)
    return ZX_ERR_INVALID_ARGS;

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<ProcessDispatcher> process;
  zx_status_t status = up->handle_table().GetDispatcherWithRights(
      *up, handle, ZX_RIGHT_READ | ZX_RIGHT_WRITE, &process);
  if (status != ZX_OK)
    return status;

  auto aspace = process->aspace_at(vaddr);
  if (!aspace)
    return ZX_ERR_BAD_STATE;

  auto region = aspace->FindRegion(vaddr);
  if (!region)
    return ZX_ERR_NOT_FOUND;

  auto vm_mapping = region->as_vm_mapping();
  if (!vm_mapping)
    return ZX_ERR_NOT_FOUND;

  auto vmo = vm_mapping->vmo();
  if (!vmo)
    return ZX_ERR_NOT_FOUND;

  uint64_t offset;
  {
    Guard<CriticalMutex> guard{vm_mapping->lock()};
    offset = vaddr - vm_mapping->base_locked() + vm_mapping->object_offset_locked();
    // TODO(https://fxbug.dev/42106495): While this limits reading to the mapped address space of
    // this VMO, it should be reading from multiple VMOs, not a single one.
    // Additionally, it is racy with the mapping going away.
    buffer_size =
        ktl::min(buffer_size, vm_mapping->size_locked() - (vaddr - vm_mapping->base_locked()));
  }
  size_t out_actual = 0;
  zx_status_t st = vmo->ReadUser(buffer.reinterpret<char>(), offset, buffer_size,
                                 VmObjectReadWriteOptions::TrimLength, &out_actual);
  if (st != ZX_OK) {
    // Do not write |out_actual| to |actual| on error
    return st;
  }
  if (out_actual == 0) {
    // If our partial read returned 0 bytes, it means that offset is past the end of the VMO.
    return ZX_ERR_OUT_OF_RANGE;
  }

  return actual.copy_to_user(out_actual);
}

// zx_status_t zx_process_write_memory
zx_status_t sys_process_write_memory(zx_handle_t handle, zx_vaddr_t vaddr,
                                     user_in_ptr<const void> buffer, size_t buffer_size,
                                     user_out_ptr<size_t> actual) {
  LTRACEF("vaddr 0x%" PRIxPTR ", size %zu\n", vaddr, buffer_size);

  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!buffer)
    return ZX_ERR_INVALID_ARGS;
  if (buffer_size == 0 || buffer_size > kMaxDebugWriteBlock)
    return ZX_ERR_INVALID_ARGS;

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<ProcessDispatcher> process;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &process);
  if (status != ZX_OK)
    return status;

  auto aspace = process->aspace_at(vaddr);
  if (!aspace)
    return ZX_ERR_BAD_STATE;

  auto region = aspace->FindRegion(vaddr);
  if (!region)
    return ZX_ERR_NOT_FOUND;

  auto vm_mapping = region->as_vm_mapping();
  if (!vm_mapping)
    return ZX_ERR_NOT_FOUND;

  auto vmo = vm_mapping->vmo();
  if (!vmo)
    return ZX_ERR_NOT_FOUND;

  if (VDso::vmo_is_vdso(vmo)) {
    // Don't allow writes to the vDSO.
    return ZX_ERR_ACCESS_DENIED;
  }

  uint64_t offset;
  {
    Guard<CriticalMutex> guard{vm_mapping->lock()};
    offset = vaddr - vm_mapping->base_locked() + vm_mapping->object_offset_locked();
    // TODO(https://fxbug.dev/42106495): While this limits writing to the mapped address space of
    // this VMO, it should be writing to multiple VMOs, not a single one.
    // Additionally, it is racy with the mapping going away.
    buffer_size =
        ktl::min(buffer_size, vm_mapping->size_locked() - (vaddr - vm_mapping->base_locked()));
  }
  size_t out_actual = 0;
  zx_status_t st = vmo->WriteUser(buffer.reinterpret<const char>(), offset, buffer_size,
                                  VmObjectReadWriteOptions::TrimLength, &out_actual,
                                  /*on_bytes_transferred=*/nullptr);
  if (st != ZX_OK) {
    // Do not write |out_actual| to |actual| on error~
    return st;
  }
  if (out_actual == 0) {
    // If our partial write returned 0 bytes, it means that offset is past the end of the VMO.
    return ZX_ERR_OUT_OF_RANGE;
  }

  return actual.copy_to_user(out_actual);
}

// helper routine for sys_task_kill
template <typename T>
static zx_status_t kill_task(fbl::RefPtr<Dispatcher> dispatcher) {
  auto task = DownCastDispatcher<T>(&dispatcher);
  if (!task)
    return ZX_ERR_WRONG_TYPE;

  task->Kill(ZX_TASK_RETCODE_SYSCALL_KILL);
  return ZX_OK;
}

// zx_status_t zx_task_kill
zx_status_t sys_task_kill(zx_handle_t task_handle) {
  LTRACEF("handle %x\n", task_handle);

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<Dispatcher> dispatcher;
  auto status =
      up->handle_table().GetDispatcherWithRights(*up, task_handle, ZX_RIGHT_DESTROY, &dispatcher);
  if (status != ZX_OK)
    return status;

  // See if it's a process or job and dispatch accordingly. Killing a thread is not supported.
  switch (dispatcher->get_type()) {
    case ZX_OBJ_TYPE_JOB:
      return kill_task<JobDispatcher>(ktl::move(dispatcher));
    case ZX_OBJ_TYPE_PROCESS:
      return kill_task<ProcessDispatcher>(ktl::move(dispatcher));
    default:
      return ZX_ERR_WRONG_TYPE;
  }
}
#endif

zx_status_t zx_job_create(zx_handle_t parent_job, uint32_t options, zx_handle_t* out) {
  LTRACEF("parent: %x\n", parent_job);

  if (options != 0u)
    return ZX_ERR_INVALID_ARGS;

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  fbl::RefPtr<JobDispatcher> parent;
  zx_status_t status =
      handle_table(up).GetDispatcherWithRights(*up, parent_job, ZX_RIGHT_MANAGE_JOB, &parent);
  if (status != ZX_OK) {
    return status;
  }

  KernelHandle<JobDispatcher> handle;
  zx_rights_t rights;
  status = JobDispatcher::Create(options, ktl::move(parent), &handle, &rights);
  if (status == ZX_OK)
    status = MakeAndAddHandle(ktl::move(handle), rights, out);
  return status;
}

#if 0
template <typename TPolicy>
static zx_status_t job_set_policy_basic(zx_handle_t handle, uint32_t options,
                                        user_in_ptr<const void> _policy, uint32_t count) {
  if ((options != ZX_JOB_POL_RELATIVE) && (options != ZX_JOB_POL_ABSOLUTE)) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (!_policy || (count == 0u) || (count > 32u)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  fbl::InlineArray<TPolicy, kPolicyBasicInlineCount> policy(&ac, count);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto status = _policy.reinterpret<const TPolicy>().copy_array_from_user(policy.get(), count);
  if (status != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<JobDispatcher> job;
  status = up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_SET_POLICY, &job);
  if (status != ZX_OK) {
    return status;
  }

  return job->SetBasicPolicy(options, policy.get(), policy.size());
}

static zx_status_t job_set_policy_timer_slack(zx_handle_t handle, uint32_t options,
                                              user_in_ptr<const void> _policy, uint32_t count) {
  if (options != ZX_JOB_POL_RELATIVE) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (!_policy || (count != 1u)) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx_policy_timer_slack slack_policy;
  auto status = _policy.reinterpret<const zx_policy_timer_slack>().copy_from_user(&slack_policy);
  if (status != ZX_OK) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<JobDispatcher> job;
  status = up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_SET_POLICY, &job);
  if (status != ZX_OK) {
    return status;
  }

  return job->SetTimerSlackPolicy(slack_policy);
}

// zx_status_t zx_job_set_policy
zx_status_t sys_job_set_policy(zx_handle_t handle, uint32_t options, uint32_t topic,
                               user_in_ptr<const void> _policy, uint32_t count) {
  switch (topic) {
    case ZX_JOB_POL_BASIC_V1:
      return job_set_policy_basic<zx_policy_basic_v1>(handle, options, _policy, count);
    case ZX_JOB_POL_BASIC_V2:
      return job_set_policy_basic<zx_policy_basic_v2>(handle, options, _policy, count);
    case ZX_JOB_POL_TIMER_SLACK:
      return job_set_policy_timer_slack(handle, options, _policy, count);
    default:
      return ZX_ERR_INVALID_ARGS;
  };
}

zx_status_t sys_job_set_critical(zx_handle_t job_handle, uint32_t options,
                                 zx_handle_t process_handle) {
  bool retcode_nonzero = false;
  if (options == ZX_JOB_CRITICAL_PROCESS_RETCODE_NONZERO) {
    retcode_nonzero = true;
  } else if (options != 0u) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<JobDispatcher> job;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, job_handle, ZX_RIGHT_DESTROY, &job);
  if (status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<ProcessDispatcher> process;
  status = up->handle_table().GetDispatcherWithRights(*up, process_handle, ZX_RIGHT_WAIT, &process);
  if (status != ZX_OK) {
    return status;
  }

  return process->SetCriticalToJob(ktl::move(job), retcode_nonzero);
}
#endif
