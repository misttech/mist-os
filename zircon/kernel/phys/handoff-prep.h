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
#include <ktl/algorithm.h>
#include <ktl/byte.h>
#include <ktl/concepts.h>
#include <ktl/initializer_list.h>
#include <ktl/pair.h>
#include <ktl/span.h>
#include <ktl/tuple.h>
#include <ktl/utility.h>
#include <phys/handoff-ptr.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/new.h>
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
  [[noreturn]] void DoHandoff(const ElfImage& kernel, UartDriver& uart, ktl::span<ktl::byte> zbi,
                              const KernelStorage::Bootfs& kernel_package,
                              const ArchPatchInfo& patch_info);

  // Add an additonal, generic VMO to be simply published to userland.  The
  // kernel proper won't ever look at it.
  void PublishExtraVmo(PhysVmo&& vmo);

 private:
  // Comprises a list in scratch memory of the pending VM objects so they can
  // be packed into a single array at the end (via NewFromList()).
  template <ktl::derived_from<PhysVmObject> VmObject>
  struct HandoffVmObject : public fbl::SinglyLinkedListable<HandoffVmObject<VmObject>*> {
    static HandoffVmObject* New(VmObject obj) {
      fbl::AllocChecker ac;
      HandoffVmObject* handoff_obj =
          new (gPhysNew<memalloc::Type::kPhysScratch>, ac) HandoffVmObject;
      ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu scratch bytes for hand-off VM object",
                    sizeof(*handoff_obj));
      handoff_obj->object = ktl::move(obj);
      return handoff_obj;
    }

    VmObject object;
  };

  template <typename VmObject>
  using HandoffVmObjectList = fbl::SizedSinglyLinkedList<HandoffVmObject<VmObject>*>;

  using HandoffVmo = HandoffVmObject<PhysVmo>;
  using HandoffVmoList = HandoffVmObjectList<PhysVmo>;

  using HandoffVmar = HandoffVmObject<PhysVmar>;
  using HandoffVmarList = HandoffVmObjectList<PhysVmar>;

  using HandoffMapping = HandoffVmObject<PhysMapping>;
  using HandoffMappingList = HandoffVmObjectList<PhysMapping>;

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

  // TODO(https://fxbug.dev/42164859): kMmio too.
  enum class MappingType { kNormal };

  // A convenience class for building up a PhysVmar.
  class PhysVmarPrep {
   public:
    constexpr PhysVmarPrep() = default;

    // Creates the provided mapping and publishes it within the associated VMAR
    // being built up.
    void PublishMapping(PhysMapping mapping, MappingType type) {
      ZX_DEBUG_ASSERT(vmar_.base <= mapping.vaddr);
      ZX_DEBUG_ASSERT(mapping.vaddr_end() <= vmar_.end());
      CreateMapping(mapping, type);
      mappings_.push_front(HandoffMapping::New(ktl::move(mapping)));
    }

    // Publishes the PhysVmar in the hand-off.
    void Publish() && {
      prep_->NewFromList(vmar_.mappings, ktl::move(mappings_));
      prep_->vmars_.push_front(HandoffVmar::New(ktl::move(vmar_)));
      prep_ = nullptr;
    }

   private:
    friend class HandoffPrep;

    explicit PhysVmarPrep(HandoffPrep* prep, ktl::string_view name, uintptr_t base, size_t size)
        : prep_(prep), vmar_{.base = base, .size = size} {
      vmar_.set_name(name);
    }

    HandoffPrep* prep_ = nullptr;
    PhysVmar vmar_;
    HandoffMappingList mappings_;
  };

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

  static void CreateMapping(const PhysMapping& mapping, MappingType type);

  // Packs a list of pending VM objects into a single hand-off span in sorted
  // order.
  template <typename VmObject>
  ktl::span<VmObject> NewFromList(PhysHandoffTemporarySpan<const VmObject>& span,
                                  HandoffVmObjectList<VmObject> list) {
    fbl::AllocChecker ac;
    ktl::span objects = New(span, ac, list.size());
    ZX_ASSERT_MSG(ac.check(), "cannot allocate %zu * %zu-byte VM object span", list.size(),
                  sizeof(VmObject));
    ZX_DEBUG_ASSERT(objects.size() == list.size());

    for (VmObject& obj : objects) {
      obj = ktl::move(list.pop_front()->object);
    }
    ktl::sort(objects.begin(), objects.end());
    return objects;
  }

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

  // Constructs a prep object for publishing a PhysVmar.
  PhysVmarPrep PrepareVmarAt(ktl::string_view name, uintptr_t base, size_t size) {
    PhysVmarPrep prep;
    prep.prep_ = this;
    prep.vmar_ = PhysVmar{.base = base, .size = size};
    prep.vmar_.set_name(name);
    return prep;
  }

  // Publishes a PhysVmar with a single mapping covering its extent.
  void PublishSingleMappingVmar(PhysMapping mapping, MappingType type);

  // Do final handoff of the VM object lists.  The contents are already in
  // place so this does not invalidate any pointers to the objects (e.g., from
  // PublishExtraVmo).
  void FinishVmObjects();

  // Normalizes and publishes RAM and the allocations of interest to the kernel.
  //
  // This must be the very last set-up routine called within DoHandoff().
  void SetMemory();

  void ConstructKernelAddressSpace(const ElfImage& kernel);

  PhysHandoff* handoff_ = nullptr;
  zbitl::Image<Allocation> mexec_image_;
  HandoffVmarList vmars_;
  HandoffVmoList extra_vmos_;
};

#endif  // ZIRCON_KERNEL_PHYS_HANDOFF_PREP_H_
