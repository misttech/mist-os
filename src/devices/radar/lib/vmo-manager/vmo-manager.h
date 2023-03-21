// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.radar/cpp/wire.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fit/result.h>
#include <lib/stdcompat/span.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <stdint.h>

#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "src/lib/vmo_store/vmo_store.h"

namespace radar {

// Thread-safe utility class for keeping track of registered VMOs, their VMARs, and lock states.
class VmoManager {
 public:
  explicit VmoManager(size_t minimum_vmo_size);

  ~VmoManager();

  // Gets the next unlocked VMO, locks it, writes the provided data to it, and returns the VMO ID.
  // Returns an error if no VMOs are unlocked or if `data` is too large.
  fit::result<fuchsia_hardware_radar::StatusCode, uint32_t> WriteUnlockedVmoAndGetId(
      cpp20::span<const uint8_t> data);

  // Unlocks the VMO corresponding to `vmo_id`.
  void UnlockVmo(uint32_t vmo_id);

  // Registers the given VMOs. The manager is reset to its state from before this call if any of the
  // VMOs could not be registered.
  fuchsia_hardware_radar::StatusCode RegisterVmos(fidl::VectorView<const uint32_t> vmo_ids,
                                                  fidl::VectorView<zx::vmo> vmos);
  fuchsia_hardware_radar::StatusCode RegisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                                  fidl::VectorView<zx::vmo> vmos);
  fit::result<fuchsia_hardware_radar::StatusCode> RegisterVmos(const std::vector<uint32_t>& vmo_ids,
                                                               std::vector<zx::vmo> vmos);

  // Unregisters and returns the VMOs corresponding to `vmo_ids`. If any of the VMOs could not be
  // unregistered, `out_vmos` is not populated, and the state of the manager is not changed.
  fuchsia_hardware_radar::StatusCode UnregisterVmos(fidl::VectorView<const uint32_t> vmo_ids,
                                                    fidl::VectorView<zx::vmo> out_vmos);
  fuchsia_hardware_radar::StatusCode UnregisterVmos(fidl::VectorView<uint32_t> vmo_ids,
                                                    fidl::VectorView<zx::vmo> out_vmos);
  fit::result<fuchsia_hardware_radar::StatusCode, std::vector<zx::vmo>> UnregisterVmos(
      const std::vector<uint32_t>& vmo_ids);

 private:
  struct VmoMeta : public fbl::DoublyLinkedListable<VmoMeta*, fbl::NodeOptions::AllowMove> {
    VmoMeta() : fbl::DoublyLinkedListable<VmoMeta*, kNodeOptions>() {}
    uint32_t vmo_id;
    cpp20::span<uint8_t> vmo_data;
  };

  using VmoStore = vmo_store::VmoStore<vmo_store::HashTableStorage<uint32_t, VmoMeta>>;

  const size_t minimum_vmo_size_;

  fbl::Mutex lock_;
  // Doubly linked lists to keep track of which VMOs are locked and which are unlocked. Entries
  // (VmoMeta*) are owned by registered_vmos_.
  fbl::DoublyLinkedList<VmoMeta*> locked_vmos_ TA_GUARDED(lock_);
  fbl::DoublyLinkedList<VmoMeta*> unlocked_vmos_ TA_GUARDED(lock_);
  VmoStore registered_vmos_ TA_GUARDED(lock_);
};

}  // namespace radar
