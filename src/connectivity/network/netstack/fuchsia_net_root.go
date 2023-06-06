// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//go:build !build_with_native_toolchain

package netstack

import (
	// #include "zircon/process.h"
	"C"

	"syscall/zx"
	"syscall/zx/fidl"

	"go.fuchsia.dev/fuchsia/src/lib/component"
	syslog "go.fuchsia.dev/fuchsia/src/lib/syslog/go"

	"fidl/fuchsia/net/interfaces/admin"
	"fidl/fuchsia/net/root"

	"gvisor.dev/gvisor/pkg/tcpip"
)

var _ root.InterfacesWithCtx = (*rootInterfacesImpl)(nil)

type rootInterfacesImpl struct {
	ns *Netstack
}

func (ci *rootInterfacesImpl) GetAdmin(_ fidl.Context, nicid uint64, request admin.ControlWithCtxInterfaceRequest) error {
	{
		nicid := tcpip.NICID(nicid)
		nicInfo, ok := ci.ns.stack.NICInfo()[nicid]
		if !ok {
			if err := component.CloseWithEpitaph(request.Channel, zx.ErrNotFound); err != nil {
				_ = syslog.WarnTf(root.InterfacesName, "GetAdmin(%d) close error: %s", nicid, err)
			}
			return nil
		}

		ifs := nicInfo.Context.(*ifState)
		ifs.addAdminConnection(request, false /* strong */)
		return nil
	}
}
