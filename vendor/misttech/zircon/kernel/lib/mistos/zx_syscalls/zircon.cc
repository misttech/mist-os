// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/counters.h>
#include <lib/crypto/global_prng.h>
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
#include <zircon/syscalls/log.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <explicit-memory/bytes.h>
#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/auto_lock.h>
#include <kernel/thread.h>
#include <ktl/algorithm.h>
#include <ktl/atomic.h>
#include <object/event_dispatcher.h>
#include <object/event_pair_dispatcher.h>
#include <object/handle.h>
#include <object/log_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/resource.h>
#include <object/thread_dispatcher.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

// KCOUNTER(syscalls_zx_ticks_get, "syscalls.zx_ticks_get")
// KCOUNTER(syscalls_zx_clock_get_monotonic, "syscalls.zx_clock_get_monotonic")
// KCOUNTER(syscalls_zx_nanosleep, "syscalls.zx_nanosleep")
// KCOUNTER(syscalls_zx_nanosleep_zero_duration, "syscalls.zx_nanosleep_zero_duration")

constexpr size_t kMaxCPRNGDraw = ZX_CPRNG_DRAW_MAX_LEN;
constexpr size_t kMaxCPRNGSeed = ZX_CPRNG_ADD_ENTROPY_MAX_LEN;

zx_status_t zx_nanosleep(zx_time_t deadline) {
  LTRACEF("nseconds %" PRIi64 "\n", deadline);
  // kcounter_add(syscalls_zx_nanosleep, 1);

  if (deadline <= 0) {
    // kcounter_add(syscalls_zx_nanosleep_zero_duration, 1);
    return ZX_OK;
  }

  const zx_time_t now = current_time();

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
    const Deadline slackDeadline(deadline, up->GetTimerSlackPolicy());

    ThreadDispatcher::AutoBlocked by(ThreadDispatcher::Blocked::SLEEPING);

    // This syscall is declared as "blocking", so a higher layer will automatically
    // retry if we return ZX_ERR_INTERNAL_INTR_RETRY.
    return Thread::Current::SleepEtc(slackDeadline, Interruptible::Yes, now);
  } else {
    const Deadline slackDeadline(deadline, TimerSlack::none());

    // This syscall is declared as "blocking", so a higher layer will automatically
    // retry if we return ZX_ERR_INTERNAL_INTR_RETRY.
    return Thread::Current::SleepEtc(slackDeadline, Interruptible::Yes, now);
  }
}

#if 0
zx_time_t sys_clock_get_monotonic_via_kernel() {
  kcounter_add(syscalls_zx_clock_get_monotonic, 1);
  return current_time();
}
#endif

zx_ticks_t zx_ticks_get() {
  // kcounter_add(syscalls_zx_ticks_get, 1);
  return current_ticks();
}

// zx_status_t zx_event_create
zx_status_t zx_event_create(uint32_t options, zx_handle_t* out) {
  LTRACEF("options 0x%x\n", options);

  if (options != 0u)
    return ZX_ERR_INVALID_ARGS;

  KernelHandle<EventDispatcher> kernel_handle;
  zx_rights_t rights;

  zx_status_t result = EventDispatcher::Create(options, &kernel_handle, &rights);
  if (result != ZX_OK) {
    return result;
  }
  return MakeAndAddHandle(ktl::move(kernel_handle), rights, out);
}

#if 0
// zx_status_t zx_eventpair_create
zx_status_t sys_eventpair_create(uint32_t options, zx_handle_t* out0, zx_handle_t* out1) {
  if (options != 0u)  // No options defined/supported yet.
    return ZX_ERR_NOT_SUPPORTED;

  auto up = ProcessDispatcher::GetCurrent();
  zx_status_t res = up->EnforceBasicPolicy(ZX_POL_NEW_EVENTPAIR);
  if (res != ZX_OK)
    return res;

  KernelHandle<EventPairDispatcher> handle0, handle1;
  zx_rights_t rights;
  zx_status_t result = EventPairDispatcher::Create(&handle0, &handle1, &rights);

  if (result == ZX_OK)
    result = up->MakeAndAddHandle(ktl::move(handle0), rights, out0);
  if (result == ZX_OK)
    result = up->MakeAndAddHandle(ktl::move(handle1), rights, out1);

  return result;
}
#endif

zx_status_t zx_debuglog_create(zx_handle_t rsrc, uint32_t options, zx_handle_t* out) {
  LTRACEF("options 0x%x\n", options);
  // To support allowing the libc dynamic linker to emit log messages even
  // before process bootstrap is complete, we allow creating a debuglog with
  // options == 0 (write-only) without yet having a valid `rsrc` handle.
  // Otherwise, we should require a valid `rsrc` handle.
  // Inversely: if a resource handle is given, or if `options` is nonzero,
  // require that `rsrc` be a valid debuglog resource handle.
  if (rsrc != ZX_HANDLE_INVALID) {
    if (zx_status_t status = validate_resource_kind_base(rsrc, ZX_RSRC_KIND_SYSTEM,
                                                         ZX_RSRC_SYSTEM_DEBUGLOG_BASE) != ZX_OK)
      return status;
  } else {
    if (options != 0)
      return ZX_ERR_BAD_HANDLE;
  }

  // Ensure only valid options were given provided. The only valid flag is currently
  // ZX_LOG_FLAGS_READABLE.
  if ((options & ZX_LOG_FLAG_READABLE) != options) {
    return ZX_ERR_INVALID_ARGS;
  }

  // create a Log dispatcher
  KernelHandle<LogDispatcher> handle;
  zx_rights_t rights;
  zx_status_t result = LogDispatcher::Create(options, &handle, &rights);
  if (result != ZX_OK)
    return result;

  // by default log objects are write-only
  // as readable logs are more expensive
  if (options & ZX_LOG_FLAG_READABLE) {
    rights |= ZX_RIGHT_READ;
  }

  // create a handle and attach the dispatcher to it
  return MakeAndAddHandle(ktl::move(handle), rights, out);
}

zx_status_t zx_debuglog_write(zx_handle_t log_handle, uint32_t options, const void* ptr,
                              size_t len) {
  LTRACEF("log handle %x, opt %x, ptr 0x%p, len %zu\n", log_handle, options, ptr, len);

  len = len > DLOG_MAX_DATA ? DLOG_MAX_DATA : len;

  if (options & (~ZX_LOG_FLAGS_MASK))
    return ZX_ERR_INVALID_ARGS;

  ProcessDispatcher* up = nullptr;
  bool is_user_thread = ThreadDispatcher::GetCurrent() != nullptr;
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  fbl::RefPtr<LogDispatcher> log;
  zx_status_t status =
      handle_table(up).GetDispatcherWithRights(*up, log_handle, ZX_RIGHT_WRITE, &log);
  if (status != ZX_OK)
    return status;

  char buf[DLOG_MAX_RECORD];
  memcpy(buf, ptr, len);

  return log->Write(DEBUGLOG_INFO, options, {buf, len});
}

#if 0
// Converts a dlog_record_t into a zx_log_record_t and copies it out to user memory.
//
// Copies at most |len| bytes to |dst|.
static zx::result<size_t> CopyOutLogRecord(const dlog_record_t& internal_record,
                                           user_out_ptr<zx_log_record_t> dst, size_t len) {
  zx_log_record_t external_record{};
  external_record.sequence = internal_record.hdr.sequence;
  external_record.datalen = internal_record.hdr.datalen;
  external_record.severity = internal_record.hdr.severity;
  external_record.flags = internal_record.hdr.flags;
  external_record.timestamp = internal_record.hdr.timestamp;
  external_record.pid = internal_record.hdr.pid;
  external_record.tid = internal_record.hdr.tid;

  // The user's buffer may not be large enough to hold the zx_log_record_t let
  // alone the flexible array member that follows.
  //
  //
  zx_status_t status;
  if (len < sizeof(zx_log_record_t)) {
    // Not enough space to copy the whole struct so we must treat it as an array
    // of bytes instead.
    size_t to_copy = ktl::min(len, sizeof(external_record));
    auto src = reinterpret_cast<const char*>(&external_record);
    status = dst.reinterpret<char>().copy_array_to_user(src, to_copy);
    if (status == ZX_OK) {
      return zx::ok(to_copy);
    }
    return zx::error(status);
  }

  // There's enough space for the struct so copy it as is.  By not casting to an
  // array, we benefit from user_copy's static_asserts that verify the type
  // (zx_log_record_t) is ABI-safe.
  status = dst.copy_to_user(external_record);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  size_t amount_copied = sizeof(external_record);
  len -= amount_copied;

  // Copy out as much of the flexible array member as will fit.
  size_t to_copy = ktl::min(len, static_cast<size_t>(external_record.datalen));
  status = dst.reinterpret<char>()
               .byte_offset(amount_copied)
               .copy_array_to_user(internal_record.data, to_copy);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  amount_copied += to_copy;

  return zx::ok(amount_copied);
}

// zx_status_t zx_debuglog_read
zx_status_t sys_debuglog_read(zx_handle_t log_handle, uint32_t options, user_out_ptr<void> ptr,
                              size_t len) {
  LTRACEF("log handle %x, opt %x, ptr 0x%p, len %zu\n", log_handle, options, ptr.get(), len);

  if (options != 0)
    return ZX_ERR_INVALID_ARGS;

  auto up = ProcessDispatcher::GetCurrent();

  fbl::RefPtr<LogDispatcher> log;
  zx_status_t status =
      up->handle_table().GetDispatcherWithRights(*up, log_handle, ZX_RIGHT_READ, &log);
  if (status != ZX_OK)
    return status;

  dlog_record_t record{};
  size_t actual;
  if ((status = log->Read(options, &record, &actual)) < 0) {
    return status;
  }

  zx::result<size_t> result = CopyOutLogRecord(record, ptr.reinterpret<zx_log_record_t>(), len);
  if (result.is_error()) {
    return result.error_value();
  }

  return static_cast<zx_status_t>(result.value());
}
#endif

zx_status_t zx_cprng_draw_once(void* buffer, size_t len) {
  if (len > kMaxCPRNGDraw)
    return ZX_ERR_INVALID_ARGS;

  uint8_t kernel_buf[kMaxCPRNGDraw];
  // Ensure we get rid of the stack copy of the random data as this function returns.
  explicit_memory::ZeroDtor<uint8_t> zero_guard(kernel_buf, sizeof(kernel_buf));

  auto prng = crypto::global_prng::GetInstance();
  ASSERT(prng->is_thread_safe());
  prng->Draw(kernel_buf, len);

  memcpy(buffer, kernel_buf, len);
  return ZX_OK;
}

zx_status_t zx_cprng_add_entropy(const void* buffer, size_t buffer_size) {
  if (buffer_size > kMaxCPRNGSeed)
    return ZX_ERR_INVALID_ARGS;

  uint8_t kernel_buf[kMaxCPRNGSeed];
  // Ensure we get rid of the stack copy of the entropy as this function
  // returns.
  explicit_memory::ZeroDtor<uint8_t> zero_guard(kernel_buf, sizeof(kernel_buf));

  memcpy(kernel_buf, buffer, buffer_size);

  auto prng = crypto::global_prng::GetInstance();
  ASSERT(prng->is_thread_safe());
  prng->AddEntropy(kernel_buf, buffer_size);

  return ZX_OK;
}
