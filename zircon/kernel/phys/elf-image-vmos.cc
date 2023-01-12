// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/llvm-profdata/llvm-profdata.h>

#include <ktl/string_view.h>
#include <phys/elf-image.h>
#include <phys/handoff.h>
#include <phys/symbolize.h>

#include <ktl/enforce.h>

namespace {

// Construct a VMO name for the instrumentation data from the debugdata
// protocol sink name, the module name and the canonical file name suffix for
// this sink.
// TODO(mcgrathr): Currently this matches the old data/phys/... format and
// doesn't with the sink name.  Later change maybe to
// "i/sink-name/module+suffix" but omit suffix entirely if it doesn't all fit?
template <const ktl::string_view& kSinkName, const ktl::string_view& kSuffix>
constexpr PhysVmo::Name VmoName(ktl::string_view module_name) {
  PhysVmo::Name vmo_name{};
  ktl::span<char> buffer{vmo_name};
  for (ktl::string_view str : {"data/phys/"sv, module_name, kSuffix}) {
    buffer = buffer.subspan(str.copy(buffer.data(), buffer.size() - 1));
    if (buffer.empty()) {
      break;
    }
  }
  return vmo_name;
}

// Returns a pointer into the array that was passed by reference.
constexpr ktl::string_view VmoNameString(const PhysVmo::Name& name) {
  ktl::string_view str(name.data(), name.size());
  return str.substr(0, str.find_first_of('\0'));
}

template <const ktl::string_view& kSinkName, const ktl::string_view& kSuffix,
          const ktl::string_view& kAnnounce>
ktl::span<ktl::byte> PublishInstrumentationVmo(const ElfImage& module,
                                               ElfImage::PublishVmoFunction& publish_vmo,
                                               size_t content_size) {
  PhysVmo::Name vmo_name = VmoName<kSinkName, kSuffix>(module.name());
  ktl::string_view name = VmoNameString(vmo_name);
  gSymbolize->DumpFile(kSinkName, name, LlvmProfdata::kAnnounce, content_size);
  return publish_vmo(name, content_size);
}

}  // namespace

// TODO(mcgrathr): This code must actually be repeated separately in each
// module so it can access its own per-module data with link-time references.
// For now this only actually gets used in the physboot main ZBI executable.
// Ideally the per-module code would be the absolute minimum, e.g. like
// PublishSelfLlvmProfdata but without the PublishInstrumentationVmo code and
// RODATA strings for the names.
void ElfImage::PublishInstrumentation(PublishVmoFunction publish_vmo) const {
  ZX_DEBUG_ASSERT(build_id_);

  PublishSelfLlvmProfdata(publish_vmo);
}

// Publish llvm-profdata if present.
void ElfImage::PublishSelfLlvmProfdata(PublishVmoFunction& publish_vmo) const {
  LlvmProfdata profdata;
  profdata.Init(build_id_->desc);
  if (profdata.size_bytes() > 0) {
    ktl::span buffer =
        PublishInstrumentationVmo<LlvmProfdata::kDataSinkName, LlvmProfdata::kFileSuffix,
                                  LlvmProfdata::kAnnounce>(*this, publish_vmo,
                                                           profdata.size_bytes());

    // Copy the fixed data and initial counter values and then start updating
    // the handoff data in place.
    ktl::span counters = profdata.WriteFixedData(buffer);
    profdata.CopyCounters(counters);
    LlvmProfdata::UseCounters(counters);
  }
}
