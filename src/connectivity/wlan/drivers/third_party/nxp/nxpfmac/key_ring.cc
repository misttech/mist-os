// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/key_ring.h"

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/ioctl_adapter.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/mlan.h"

namespace {

bool is_broadcast_mac(const uint8_t* addr) {
  constexpr uint8_t kBroadcastMac[] = {0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF};

  return memcmp(addr, kBroadcastMac, sizeof(kBroadcastMac)) == 0;
}

}  // namespace

namespace wlan::nxpfmac {

KeyRing::KeyRing(IoctlAdapter* ioctl_adapter, uint32_t bss_index)
    : ioctl_adapter_(ioctl_adapter), bss_index_(bss_index) {}

KeyRing::~KeyRing() {
  const zx_status_t status = RemoveAllKeys();
  if (status != ZX_OK) {
    NXPF_ERR("Failed to remove all keys: %s", zx_status_get_string(status));
  }
}

zx_status_t KeyRing::AddKey(const fuchsia_wlan_fullmac::wire::SetKeyDescriptor& key) {
  if (key.key.count() == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{
          .sub_command = MLAN_OID_SEC_CFG_ENCRYPT_KEY,
          .param{.encrypt_key{.key_index = key.key_id, .key_flags = KEY_FLAG_SET_TX_KEY}}});

  auto& encrypt_key = request.UserReq().param.encrypt_key;

  if (key.key.count() > sizeof(encrypt_key.key_material)) {
    NXPF_ERR("Key length %zu exceeds maximum possible size of %zu", key.key.count(),
             sizeof(encrypt_key.key_material));
    return ZX_ERR_INVALID_ARGS;
  }

  memcpy(encrypt_key.key_material, key.key.data(), key.key.count());
  encrypt_key.key_len = static_cast<uint32_t>(key.key.count());
  memcpy(encrypt_key.mac_addr, key.address.data(), sizeof(encrypt_key.mac_addr));
  if (is_broadcast_mac(key.address.data())) {
    encrypt_key.key_flags |= KEY_FLAG_GROUP_KEY;
  }

  if (key.rsc) {
    // mlan has 16 bytes of rx sequence but we only have 8, make sure they go in the least
    // significant bytes.
    memcpy(&encrypt_key.pn[sizeof(encrypt_key.pn) - sizeof(key.rsc)], &key.rsc, sizeof(key.rsc));
    encrypt_key.key_flags |= KEY_FLAG_RX_SEQ_VALID;
  }

  switch (key.cipher_suite_type) {
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kWep40:
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kWep104:
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kTkip: {
      // The RX and TX parts of the key needs to be swapped around as they are currently from the
      // APs point of view. Some supplicants perform this swap so that when the key gets to this
      // point it's ready to go. Our supplicant does not do this and firmware expects us to do the
      // swap. Otherwise the key won't work.
      constexpr size_t kApToClientKeyOffset = 16;
      constexpr size_t kClientToApKeyOffset = 24;
      constexpr size_t kKeyLength = 8;
      uint8_t tmp[kKeyLength];
      memcpy(tmp, &encrypt_key.key_material[kClientToApKeyOffset], sizeof(tmp));
      memcpy(&encrypt_key.key_material[kClientToApKeyOffset],
             &encrypt_key.key_material[kApToClientKeyOffset], sizeof(tmp));
      memcpy(&encrypt_key.key_material[kApToClientKeyOffset], tmp, sizeof(tmp));
    } break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kGcmp128:
      encrypt_key.key_flags |= KEY_FLAG_GCMP;
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kGcmp256:
      encrypt_key.key_flags |= KEY_FLAG_GCMP_256;
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipCmac128:
      encrypt_key.key_flags |= KEY_FLAG_AES_MCAST_IGTK;
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipGmac128:
      encrypt_key.key_flags |= KEY_FLAG_AES_MCAST_IGTK | KEY_FLAG_GMAC_128;
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kBipGmac256:
      encrypt_key.key_flags |= KEY_FLAG_AES_MCAST_IGTK | KEY_FLAG_GMAC_256;
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kCcmp128:
      break;
    case fuchsia_wlan_ieee80211::wire::CipherSuiteType::kCcmp256:
      encrypt_key.key_flags |= KEY_FLAG_CCMP_256;
      break;
    default:
      NXPF_ERR("Unsupported cipher suite: %u", key.cipher_suite_type);
      return ZX_ERR_INVALID_ARGS;
  }

  const IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to add key: %d", io_status);
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t KeyRing::RemoveKey(uint16_t key_index, const uint8_t* addr) {
  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{
          .sub_command = MLAN_OID_SEC_CFG_ENCRYPT_KEY,
          .param{.encrypt_key{
              .key_remove = true, .key_index = key_index, .key_flags = KEY_FLAG_REMOVE_KEY}}});

  auto& encrypt_key = request.UserReq().param.encrypt_key;
  memcpy(encrypt_key.mac_addr, addr, sizeof(encrypt_key.mac_addr));

  const IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to remove key %u: %d", key_index, io_status);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

zx_status_t KeyRing::RemoveAllKeys() {
  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{.sub_command = MLAN_OID_SEC_CFG_ENCRYPT_KEY,
                      .param{.encrypt_key{.key_disable = true, .key_flags = KEY_FLAG_REMOVE_KEY}}});
  const IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to remove all keys: %d", io_status);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

zx_status_t KeyRing::EnableWepKey(uint16_t key_id) {
  IoctlRequest<mlan_ds_sec_cfg> request(
      MLAN_IOCTL_SEC_CFG, MLAN_ACT_SET, bss_index_,
      mlan_ds_sec_cfg{.sub_command = MLAN_OID_SEC_CFG_ENCRYPT_KEY,
                      .param{.encrypt_key{.key_index = key_id, .is_current_wep_key = MTRUE}}});

  const IoctlStatus io_status = ioctl_adapter_->IssueIoctlSync(&request);
  if (io_status != IoctlStatus::Success) {
    NXPF_ERR("Failed to enable WEP key: %d", io_status);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}

}  // namespace wlan::nxpfmac
