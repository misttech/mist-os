// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/instrumentation/vmo.h>
#include <lib/version.h>
#include <stdio.h>
#include <string.h>

#include <ktl/initializer_list.h>
#include <ktl/iterator.h>
#include <ktl/string_view.h>
#include <object/vm_object_dispatcher.h>
#include <phys/handoff.h>
#include <vm/vm_object_paged.h>

#include "private.h"

#include <ktl/enforce.h>

namespace {

// This object facilitates doing fprintf directly into the VMO representing
// the symbolizer markup data file.  This gets the symbolizer context for the
// kernel and then a dumpfile element for each VMO published.
class SymbolizerFile {
 public:
  SymbolizerFile() {
    zx_status_t status =
        VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, VmObjectPaged::kResizable, PAGE_SIZE,
                              AttributionObject::GetKernelAttribution(), &vmo_);
    ZX_ASSERT(status == ZX_OK);
  }

  FILE* stream() { return &stream_; }

  int Write(ktl::string_view str) {
    zx_status_t status = vmo_->Write(str.data(), pos_, str.size());
    ZX_ASSERT(status == ZX_OK);
    pos_ += str.size();
    return static_cast<int>(str.size());
  }

  // Move the VMO into a handle and return it.
  Handle* Finish() && {
    fbl::RefPtr<ContentSizeManager> content_size_manager;
    zx_status_t status = ContentSizeManager::Create(0, &content_size_manager);
    ZX_ASSERT(status == ZX_OK);

    KernelHandle<VmObjectDispatcher> handle;
    zx_rights_t rights;
    status = VmObjectDispatcher::Create(ktl::move(vmo_), ktl::move(content_size_manager),
                                        VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                        &rights);
    ZX_ASSERT(status == ZX_OK);
    status = handle.dispatcher()->set_name(kVmoName.data(), kVmoName.size());
    DEBUG_ASSERT(status == ZX_OK);
    status = handle.dispatcher()->SetContentSize(pos_);
    DEBUG_ASSERT(status == ZX_OK);
    return Handle::Make(ktl::move(handle), rights).release();
  }

 private:
  static constexpr ktl::string_view kVmoName = "data/symbolizer.log";

  fbl::RefPtr<VmObjectPaged> vmo_;
  FILE stream_{this};
  size_t pos_ = 0;
};

void PrintDumpfile(const InstrumentationDataVmo& data, ktl::initializer_list<FILE*> streams) {
  if (!data.handle) {
    return;
  }

  auto vmo = DownCastDispatcher<VmObjectDispatcher>(data.handle->dispatcher().get());

  char name_buffer[ZX_MAX_NAME_LEN];
  [[maybe_unused]] zx_status_t status = vmo->get_name(name_buffer);
  DEBUG_ASSERT(status == ZX_OK);
  ktl::string_view vmo_name{name_buffer, sizeof(name_buffer)};
  vmo_name = vmo_name.substr(0, vmo_name.find_first_of('\0'));

  size_t content_size = vmo->GetContentSize();
  size_t scaled_size = content_size / data.scale;

  for (FILE* f : streams) {
    fprintf(f, "%.*s: {{{dumpfile:%.*s:%.*s}}} maximum %zu %.*s.\n",
            static_cast<int>(data.announce.size()), data.announce.data(),
            static_cast<int>(data.sink_name.size()), data.sink_name.data(),
            static_cast<int>(vmo_name.size()), vmo_name.data(), scaled_size,
            static_cast<int>(data.units.size()), data.units.data());
  }
}

}  // namespace

zx_status_t InstrumentationData::GetVmos(Handle* handles[]) {
  // To keep the protocol with userboot simple, we always supply all the VMO
  // handles.  Slots with no instrumentation data to report will hold an empty
  // VMO with no name.  Create this the first time it's needed and then just
  // duplicate the read-only handle as needed.
  auto get_stub_vmo = [stub_vmo = fbl::RefPtr<VmObjectDispatcher>{},
                       rights = zx_rights_t{}]() mutable -> Handle* {
    KernelHandle<VmObjectDispatcher> handle(stub_vmo);
    if (!stub_vmo) {
      fbl::RefPtr<VmObjectPaged> vmo;
      zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, 0, 0,
                                                 AttributionObject::GetKernelAttribution(), &vmo);
      ZX_ASSERT(status == ZX_OK);
      fbl::RefPtr<ContentSizeManager> content_size_manager;
      status = ContentSizeManager::Create(0, &content_size_manager);
      ZX_ASSERT(status == ZX_OK);
      status = VmObjectDispatcher::Create(ktl::move(vmo), ktl::move(content_size_manager),
                                          VmObjectDispatcher::InitialMutability::kMutable, &handle,
                                          &rights);
      ZX_ASSERT(status == ZX_OK);
      rights &= ~ZX_RIGHT_WRITE;
    }
    return Handle::Make(ktl::move(handle), rights).release();
  };

  SymbolizerFile symbolizer;
  PrintSymbolizerContext(symbolizer.stream());

  // This is just a fixed list of steps but using a loop and a switch gets the
  // compiler to check that every enum case is handled.
  constexpr auto get_getter = [](Vmo idx) -> InstrumentationDataVmo (*)() {
    switch (idx) {
      case kLlvmProfdataVmo:
        return LlvmProfdataVmo;
      case kSancovVmo:
        return SancovGetPcVmo;
      case kSancovCountsVmo:
        return SancovGetCountsVmo;

        // The phys ones are done separately with no extra logging.
      case kPhysFirst ... kPhysLast:
        // The symbolizer file is done separately below since it must be last.
      case kSymbolizerVmo:
      case kVmoCount:
        break;
    }
    return nullptr;
  };
  bool have_data = false;
  for (uint32_t idx = 0; idx < kPhysFirst; ++idx) {
    if (auto getter = get_getter(static_cast<Vmo>(idx))) {
      InstrumentationDataVmo data = getter();
      if (data.handle) {
        if (!data.sink_name.empty()) {
          PrintDumpfile(data, {stdout, symbolizer.stream()});
          have_data = true;
        }
        handles[idx] = data.handle;
      } else {
        handles[idx] = get_stub_vmo();
      }
    }
  }

  // The phys VMOs just need to be published to userland.
  // They already include their own logging.
  constexpr size_t kPhysMax = kPhysLast - kPhysFirst + 1;
  ktl::span<const PhysVmo> phys_vmos = gPhysHandoff->vmos.get();
  ZX_ASSERT_MSG(phys_vmos.size() <= kPhysMax,
                "too many VMOs from phys! %zu > userboot protocol max %zu", phys_vmos.size(),
                kPhysMax);
  for (uint32_t idx = kPhysFirst; idx <= kPhysLast; ++idx) {
    const size_t phys_idx = idx - kPhysFirst;
    Handle* handle = phys_vmos.size() > phys_idx  //
                         ? MakePhysVmo(phys_vmos[phys_idx])
                         : nullptr;
    handles[idx] = handle ? handle : get_stub_vmo();
  }

  handles[kSymbolizerVmo] = have_data ? ktl::move(symbolizer).Finish() : get_stub_vmo();

  return ZX_OK;
}
