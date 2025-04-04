// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_FIFO_DISPATCHER_H_
#define ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_FIFO_DISPATCHER_H_

#include <lib/user_copy/user_ptr.h>
#include <stdint.h>
#include <zircon/rights.h>
#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <ktl/variant.h>
#include <object/dispatcher.h>
#include <object/handle.h>

class FifoDispatcher final : public PeeredDispatcher<FifoDispatcher, ZX_DEFAULT_FIFO_RIGHTS> {
 public:
  static zx_status_t Create(size_t elem_count, size_t elem_size, uint32_t options,
                            KernelHandle<FifoDispatcher>* handle0,
                            KernelHandle<FifoDispatcher>* handle1, zx_rights_t* rights);

  ~FifoDispatcher() final;

  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_FIFO; }

  zx_status_t WriteFromUser(size_t elem_size, user_in_ptr<const uint8_t> src, size_t count,
                            size_t* actual);
  zx_status_t ReadToUser(size_t elem_size, user_out_ptr<uint8_t> dst, size_t count, size_t* actual);

#if __mist_os__
  zx_status_t Write(size_t elem_size, const uint8_t* ptr, size_t count, size_t* actual);
  zx_status_t Read(size_t elem_size, uint8_t* ptr, size_t count, size_t* actual);
#endif

  // PeeredDispatcher implementation.
  void on_zero_handles_locked() TA_REQ(get_lock());
  void OnPeerZeroHandlesLocked() TA_REQ(get_lock());

 private:
  FifoDispatcher(fbl::RefPtr<PeerHolder<FifoDispatcher>> holder, uint32_t options,
                 uint32_t elem_count, uint32_t elem_size, ktl::unique_ptr<uint8_t[]> data);
  ktl::variant<zx_status_t, UserCopyCaptureFaultsResult> WriteSelfLocked(
      size_t elem_size, user_in_ptr<const uint8_t> ptr, size_t count, size_t* actual)
      TA_REQ(get_lock());
  ktl::variant<zx_status_t, UserCopyCaptureFaultsResult> ReadToUserLocked(
      size_t elem_size, user_out_ptr<uint8_t> ptr, size_t count, size_t* actual) TA_REQ(get_lock());

#if __mist_os__
  zx_status_t WriteLocked(size_t elem_size, const uint8_t* ptr, size_t count, size_t* actual)
      TA_REQ(get_lock());
  zx_status_t ReadLocked(size_t elem_size, uint8_t* ptr, size_t count, size_t* actual)
      TA_REQ(get_lock());
#endif

  const uint32_t elem_count_;
  const uint32_t elem_size_;

  uint32_t head_ TA_GUARDED(get_lock());
  uint32_t tail_ TA_GUARDED(get_lock());
  ktl::unique_ptr<uint8_t[]> data_ TA_GUARDED(get_lock());

  static constexpr uint32_t kMaxSizeBytes = ZX_FIFO_MAX_SIZE_BYTES;
};

#endif  // ZIRCON_KERNEL_OBJECT_INCLUDE_OBJECT_FIFO_DISPATCHER_H_
