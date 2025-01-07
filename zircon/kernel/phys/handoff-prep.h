// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_
#define ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_

#include <lib/fit/function.h>
#include <lib/trivial-allocator/basic-leaky-allocator.h>
#include <lib/trivial-allocator/new.h>
#include <lib/trivial-allocator/page-allocator.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/image.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_single_list.h>
#include <ktl/byte.h>
#include <ktl/initializer_list.h>
#include <ktl/move.h>
#include <ktl/pair.h>
#include <ktl/span.h>
#include <ktl/tuple.h>
#include <phys/handoff-ptr.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/uart.h>
#include <phys/zbitl-allocation.h>

struct ArchPatchInfo;
struct BootOptions;
class PhysBootTimes;
class ElfImage;
class Log;

class HandoffPrep {
 public:
  // This must be called first.
  void Init();

  // This is the main structure.  After Init has been called the pointer is
  // valid but the data is in default-constructed state.
  PhysHandoff* handoff() { return handoff_; }

  // This returns new T(args...) using the temporary handoff allocator and
  // fills in the handoff_ptr to point to it.
  template <typename T, typename... Args>
  T* New(PhysHandoffTemporaryPtr<const T>& handoff_ptr, fbl::AllocChecker& ac, Args&&... args) {
    T* ptr = new (temporary_allocator_, ac) T(ktl::forward<Args>(args)...);
    if (ptr) {
      void* generic_ptr = static_cast<void*>(ptr);
      handoff_ptr.ptr_ = reinterpret_cast<uintptr_t>(generic_ptr);
    }
    return ptr;
  }

  // Similar but for new T[n] using spans instead of pointers.
  template <typename T>
  ktl::span<T> New(PhysHandoffTemporarySpan<const T>& handoff_span, fbl::AllocChecker& ac,
                   size_t n) {
    T* ptr = new (temporary_allocator_, ac) T[n];
    if (ptr) {
      void* generic_ptr = static_cast<void*>(ptr);
      handoff_span.ptr_.ptr_ = reinterpret_cast<uintptr_t>(generic_ptr);
      handoff_span.size_ = n;
      return {ptr, n};
    }
    return {};
  }

  ktl::string_view New(PhysHandoffTemporaryString& handoff_string, fbl::AllocChecker& ac,
                       ktl::string_view str) {
    ktl::span chars = New(handoff_string, ac, str.size());
    if (chars.empty()) {
      return {};
    }
    ZX_DEBUG_ASSERT(chars.size() == str.size());
    return {chars.data(), str.copy(chars.data(), chars.size())};
  }

  // This does all the main work of preparing for the kernel, and then calls
  // `boot` to transfer control to the kernel entry point with the handoff()
  // pointer as its argument. The `boot` function should do nothing but hand
  // off to the kernel; in particular, state has already been captured from
  // `uart` so no additional printing should be done at this stage.  Init()
  // must have been called first.
  //
  // TODO(https://fxbug.dev/42164859): Pass `kernel` as a const reference,
  // which will be possible once we can jump to it at its loaded virtual
  // address.
  [[noreturn]] void DoHandoff(ElfImage& kernel, UartDriver& uart, ktl::span<ktl::byte> zbi,
                              const KernelStorage::Bootfs& kernel_package,
                              const ArchPatchInfo& patch_info);

  // Add an additonal, generic VMO to be simply published to userland.  The
  // kernel proper won't ever look at it.
  void PublishExtraVmo(PhysVmo&& vmo);

 private:
  template <memalloc::Type Type>
  class PhysPages {
   public:
    using Capability = Allocation;

    size_t page_size() const { return ZX_PAGE_SIZE; }

    [[nodiscard]] ktl::pair<void*, Capability> Allocate(size_t size) {
      fbl::AllocChecker ac;
      Allocation pages = Allocation::New(ac, Type, size, ZX_PAGE_SIZE);
      if (!ac.check()) {
        return {};
      }
      return {pages.get(), ktl::move(pages)};
    }

    void Deallocate(Capability allocation, void* ptr, size_t size) {
      ZX_DEBUG_ASSERT(ptr == allocation.get());
      ZX_DEBUG_ASSERT(size == allocation.size_bytes());
      allocation.reset();
    }

    void Release(Capability allocation, void* ptr, size_t size) {
      ZX_DEBUG_ASSERT(ptr == allocation.get());
      ZX_DEBUG_ASSERT(size == allocation.size_bytes());
      ktl::ignore = allocation.release();
    }

    void Seal(Capability, void*, size_t) { ZX_PANIC("Unexpected call to Seal::Capability"); }
  };

  template <memalloc::Type Type>
  using PageAllocationFunction = trivial_allocator::PageAllocator<PhysPages<Type>>;

  template <memalloc::Type Type>
  using Allocator = trivial_allocator::BasicLeakyAllocator<PageAllocationFunction<Type>>;

  using TemporaryAllocator = Allocator<memalloc::Type::kTemporaryPhysHandoff>;

  // A list in scratch memory of the pending PhysVmo structs so they
  // can be packed into a single array at the end.
  struct HandoffVmo : public fbl::SinglyLinkedListable<HandoffVmo*> {
    PhysVmo vmo;
  };
  using HandoffVmoList = fbl::SizedSinglyLinkedList<HandoffVmo*>;

  struct Debugdata {
    ktl::string_view announce, sink_name, vmo_name;
    size_t size_bytes = 0;
  };

  inline static TemporaryAllocator temporary_allocator_;

  // Constructs a PhysVmo from the provided information, enforcing that `data`
  // is page-aligned and that page-rounding `content_size` up yields
  // `data.size_bytes()`.
  static PhysVmo MakePhysVmo(ktl::span<const ktl::byte> data, ktl::string_view name,
                             size_t content_size);

  void SaveForMexec(const zbi_header_t& header, ktl::span<const ktl::byte> payload);

  // General arch-specific data that isn't drawn from a ZBI item.
  void ArchHandoff(const ArchPatchInfo&);

  // The arch-specific protocol for a given item.
  // Defined in //zircon/kernel/arch/$cpu/phys/arch-handoff-prep-zbi.cc.
  void ArchSummarizeMiscZbiItem(const zbi_header_t& header, ktl::span<const ktl::byte> payload);

  // Fills in handoff()->boot_options and returns the mutable reference to
  // update its fields later so that `.serial` can be transferred last.
  BootOptions& SetBootOptions(const BootOptions& boot_options);

  // Fetch things to be handed off from other files in the kernel package.
  void UsePackageFiles(KernelStorage::Bootfs kernel_package);
  void SetVersionString(ktl::string_view version);

  // Summarizes the provided data ZBI's miscellaneous simple items for the
  // kernel, filling in corresponding handoff()->item fields.  Certain fields,
  // may be cleaned after consumption for security considerations, such as
  // 'ZBI_TYPE_SECURE_ENTROPY'.
  void SummarizeMiscZbiItems(ktl::span<ktl::byte> zbi);

  // Add physboot's own instrumentation data to the handoff.  After this, the
  // live instrumented physboot code is updating the handoff data directly up
  // through the very last compiled basic block that jumps into the kernel.
  // This calls PublishExtraVmo, so it must come before FinishExtraVmos.
  void SetInstrumentation();

  // Do PublishExtraVmo with a Log buffer, which is consumed.
  void PublishLog(ktl::string_view vmo_name, Log&& log);

  // Do final handoff of the extra VMO list.  The contents are already in place,
  // so this does not invalidate pointers from PublishExtraVmo.
  void FinishExtraVmos();

  // Normalizes and publishes RAM and the allocations of interest to the kernel.
  //
  // This must be the very last set-up routine called within DoHandoff().
  void SetMemory();

  PhysHandoff* handoff_ = nullptr;
  zbitl::Image<Allocation> mexec_image_;
  HandoffVmoList extra_vmos_;
};

#endif  // ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_
