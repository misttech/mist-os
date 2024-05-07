// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "src/developer/vsock-sshd-host/data_dir.h"

#include <fidl/fuchsia.io/cpp/fidl.h>
#include <lib/fdio/namespace.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>
#include <zircon/errors.h>

#include "src/lib/files/file.h"
#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode_dir.h"

extern "C" {
// clang-format off
#include "third_party/openssh-portable/authfile.h"
#include "third_party/openssh-portable/includes.h"
#include "third_party/openssh-portable/ssherr.h"
#include "third_party/openssh-portable/sshkey.h"
// clang-format on
}

namespace {
void CopyAuthorizedKeys() {
  constexpr char kSshDir[] = "/tmp/ssh";
  constexpr char kAuthorizeKeysPath[] = "/data/ssh/authorized_keys";
  constexpr char kNewAuthorizeKeysPath[] = "/tmp/ssh/authorized_keys";

  // ignore errors, if the dir already exists, this is benign, if it does not
  // and this fails, the write will fail.
  mkdir(kSshDir, 0700);

  std::string str;
  ZX_ASSERT(files::ReadFileToString(kAuthorizeKeysPath, &str) == true);
  ZX_ASSERT(files::WriteFile(kNewAuthorizeKeysPath, str) == true);
}

void sshkey_auto_free(struct sshkey** key) { sshkey_free(*key); }

void GenerateHostKeys() {
  constexpr char key_type[] = "ed25519";
  constexpr char kPath[] = "/tmp/ssh/ssh_host_ed25519_key";

  __attribute__((cleanup(sshkey_auto_free))) struct sshkey* private_key = nullptr;
  __attribute__((cleanup(sshkey_auto_free))) struct sshkey* public_key = nullptr;

  int type = sshkey_type_from_name(key_type);

  if (int r = sshkey_generate(type, 0, &private_key); r != 0) {
    FX_LOGS(FATAL) << "sshkey_generate failed: " << ssh_err(r);
  }

  if (int r = sshkey_from_private(private_key, &public_key); r != 0) {
    FX_LOGS(FATAL) << "sshkey_from_private failed: " << ssh_err(r);
  }

  if (int r = sshkey_save_private(private_key, kPath, "", "", 1, nullptr, 0); r != 0) {
    FX_LOGS(FATAL) << "Saving key " << kPath << "failed: " << ssh_err(r);
  }
}
}  // namespace

zx::result<> BuildDataDir(async::Loop& loop, memfs::Memfs* memfs,
                          fbl::RefPtr<memfs::VnodeDir> data_dir) {
  auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  if (zx_status_t status = memfs->ServeDirectory(std::move(data_dir), std::move(server));
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Serve failed";
    return zx::error(status);
  }

  fdio_ns_t* ns;
  if (zx_status_t status = fdio_ns_get_installed(&ns); status != ZX_OK) {
    FX_LOGS(ERROR) << "fdio_ns_get_installed failed";
    return zx::error(status);
  }
  constexpr char kPath[] = "/tmp";
  if (zx_status_t status = fdio_ns_bind(ns, kPath, client.TakeChannel().release());
      status != ZX_OK) {
    FX_LOGS(ERROR) << "fdio_ns_bind failed";
    return zx::error(status);
  }
  auto unbind_cleanup = fit::defer(
      [&ns, &kPath]() { [[maybe_unused]] zx_status_t status = fdio_ns_unbind(ns, kPath); });

  std::thread t([&loop]() {
    CopyAuthorizedKeys();
    GenerateHostKeys();
    loop.Quit();
  });
  loop.Run();
  t.join();
  loop.ResetQuit();

  return zx::ok();
}
