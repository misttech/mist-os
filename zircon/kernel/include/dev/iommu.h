// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_DEV_IOMMU_H_
#define ZIRCON_KERNEL_INCLUDE_DEV_IOMMU_H_

#include <inttypes.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <sys/types.h>

#include <fbl/intrusive_double_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <vm/vm_object.h>

#define IOMMU_FLAG_PERM_READ (1 << 0)
#define IOMMU_FLAG_PERM_WRITE (1 << 1)
#define IOMMU_FLAG_PERM_EXECUTE (1 << 2)

// Type used to refer to virtual addresses presented to a device by the IOMMU.
typedef uint64_t dev_vaddr_t;

class Iommu : public fbl::RefCounted<Iommu>, public fbl::DoublyLinkedListable<fbl::RefPtr<Iommu>> {
 public:
  // Check if |bus_txn_id| is valid for this IOMMU (i.e. could be used
  // to configure a device).
  virtual bool IsValidBusTxnId(uint64_t bus_txn_id) const = 0;

  // Grant the device identified by |bus_txn_id| access to the range of
  // pages given by [offset, offset + size) in |vmo|. An opaque token that
  // represents the mapping is returned, and this token can be given to |Unmap|
  // or |QueryAddress|.
  //
  // The memory in the given range of |vmo| MUST have been pinned before
  // calling this function, and if this function returns ZX_OK,
  // MUST NOT be unpinned until after Unmap() is called on the returned range.
  //
  // |perms| defines the access permissions, using the IOMMU_FLAG_PERM_*
  // flags.
  //
  // If |size| is no more than |minimum_contiguity()|, this will never return
  // a partial mapping.
  //
  // Returns ZX_ERR_INVALID_ARGS if:
  //  |size| is zero.
  //  |offset| is not aligned to PAGE_SIZE
  // Returns ZX_ERR_OUT_OF_RANGE if [offset, offset + size) is not a valid range in |vmo|.
  // Returns ZX_ERR_NOT_FOUND if |bus_txn_id| is not valid.
  // Returns ZX_ERR_NO_RESOURCES if the mapping could not be made due to lack
  // of an available address range.
  virtual zx::result<uint64_t> Map(uint64_t bus_txn_id, const fbl::RefPtr<VmObject>& vmo,
                                   uint64_t vmo_offset, size_t size, uint32_t perms) = 0;

  // Same as Map, but with additional guarantee that this will never return a
  // partial mapping.  It will either return a single contiguous mapping or
  // return a failure.
  virtual zx::result<uint64_t> MapContiguous(uint64_t bus_txn_id, const fbl::RefPtr<VmObject>& vmo,
                                             uint64_t vmo_offset, size_t size, uint32_t perms) = 0;

  // Queries the information of a mapping created by |Map*|, identified by
  // |map_token| for the device |bus_txn_id|. The portion of the mapping to
  // query is identified by |map_offset|, with the provided |size| merely being
  // a hint of the range the caller is interested in. Fills out |vaddr| with the
  // mapped address that corresponds to the |map_offset|, and |mapped_len| with
  // the contiguity of the mapping at that point.
  // ALthough |size| is a hint, the caller is required to ensure that
  // |size + map_offset| falls within the original |size| provided to the |Map*|
  // call.
  //
  // The returned |mapped_len| could be less than, equal or greater than the
  // specified size. In the case of being less than, additional contiguous
  // ranges can be found by calling again with a new |map_offset|.
  //
  // Returns ZX_ERR_INVALID_ARGS if:
  //  |map_token| is not from a valid |Map*|.
  //  |map_offset| is not aligned to PAGE_SIZE.
  // Returns ZX_ERR_OUT_OF_RANGE if [map_offset, map_offset + size) is not a
  // valid range in the mapping.
  // Returns ZX_ERR_NOT_FOUND if |bus_txn_id| is not valid.
  virtual zx_status_t QueryAddress(uint64_t bus_txn_id, const fbl::RefPtr<VmObject>& vmo,
                                   uint64_t map_token, uint64_t map_offset, size_t size,
                                   dev_vaddr_t* vaddr, size_t* mapped_len) = 0;

  // Revoke access to the range of addresses identified by the |map_token|,
  // must have been previously returned by a |Map*| call, and the size of that
  // mapping, for the device identified by |bus_txn_id|.
  //
  // Returns ZX_ERR_INVALID_ARGS if:
  //  |size| is not a multiple of PAGE_SIZE
  // Returns ZX_ERR_NOT_FOUND if |bus_txn_id| is not valid.
  virtual zx_status_t Unmap(uint64_t bus_txn_id, uint64_t map_token, size_t size) = 0;

  // Remove all mappings for |bus_txn_id|.
  // Returns ZX_ERR_NOT_FOUND if |bus_txn_id| is not valid.
  virtual zx_status_t ClearMappingsForBusTxnId(uint64_t bus_txn_id) = 0;

  // Returns the number of bytes that Map() can guarantee, upon success, to find
  // a contiguous address range for.  This function is only returns meaningful
  // values if |IsValidBusTxnId(bus_txn_id)|.
  virtual uint64_t minimum_contiguity(uint64_t bus_txn_id) = 0;

  // Returns the total size of the space the addresses are mapped into.  This
  // function is only returns meaningful values if |IsValidBusTxnId(bus_txn_id)|.
  virtual uint64_t aspace_size(uint64_t bus_txn_id) = 0;

  virtual ~Iommu() {}
};

#endif  // ZIRCON_KERNEL_INCLUDE_DEV_IOMMU_H_
