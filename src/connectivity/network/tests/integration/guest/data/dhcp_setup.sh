#!/usr/bin/env bash
#
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# https://vaneyckt.io/posts/safer_bash_scripts_with_set_euxo_pipefail/
set -Eeuxo pipefail

# The IPv4 address must be in the subnet declared in
# `//src/connectivity/network/tests/integration/guest/dhcpv4/data/dhcpd.conf`.
# Likewise the IPv6 address must be in the subnet declared in
# `//src/connectivity/network/tests/integration/guest/dhcpv6/data/dhcpd.conf`.
declare -Ar ADDRS=( [4]="192.0.2.1/24" [6]="2001:db8::1/64" )
readonly DHCP_CONFIG_FILE="/etc/default/isc-dhcp-server"

# Mask and stop NetworkManager to prevent interference from the DHCPv4 client
# that it runs.
systemctl mask NetworkManager.service
systemctl stop NetworkManager.service

# Wait for on-boot systemd services to start, in particular this ensures that
# the instance of isc-dhcp-server started at boot has finished starting so that
# this script can successfully restart it while applying the desired
# configuration.
#
# Ignore the return code because some systemd services might fail to start
# and result in a non-zero exit code.
systemctl is-system-running --wait >/dev/null || true

# Stop the DHCP server so that we can restart it later with our own
# configuration.
systemctl stop isc-dhcp-server.service

declare -a ip

# Parse arguments:
#   -4 enables DHCPv4 server.
#   -6 enables DHCPv6 server.
while getopts "46" arg; do
  case "${arg}" in
    4)
      ip+=(4)
      ;;
    6)
      ip+=(6)
      ;;
    *)
      echo "usage: ${0} [-46]" >&2
      exit 1
      ;;
  esac
done

# If run without an IP version argument, start server with both DHCPv4 and
# DHCPv6 enabled.
if [[ "${#ip[@]}" == 0 ]]; then
  ip=(4 6)
fi
readonly ip

# The ethernet interface's name varies depending on the number of block devices
# that are added when starting the guest.  Discover the interface name prior to
# configuration.
iface_name=""

for iface_path in /sys/class/net/*; do
  candidate=$(basename "${iface_path}")
  if [[ "${candidate}" != "lo" ]]; then
    iface_name="${candidate}"
    readonly iface_name
    break
  fi
done

if [[ -z "${iface_name}" ]]; then
  echo "Unable to find an interface other than loopback" >&2
  exit 1
fi

# Overwrite the default configuration file to enable DHCPv4 or DHCPv6 on the
# selected interface as appropriate.
#
# The interface also needs an IP address or else isc-dhcp-server will not
# start.
rm "${DHCP_CONFIG_FILE}"
for ip_version in "${ip[@]}"; do
  echo "INTERFACESv${ip_version}=\"${iface_name}\"" >> "${DHCP_CONFIG_FILE}"
  ip addr add dev "${iface_name}" "${ADDRS[${ip_version}]}"
done

ip link set "${iface_name}" up

if ! systemctl restart isc-dhcp-server; then
  # If restarting fails, print out the logs to aid in troubleshooting.
  journalctl -b --no-pager -u isc-dhcp-server >&2
  exit 1
fi
systemctl is-active --quiet isc-dhcp-server
