// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package targets

import (
	"context"

	"go.fuchsia.dev/fuchsia/tools/qemu"
)

const (
	// aemuBinaryName is the name of the AEMU binary.
	aemuBinaryName = "emulator"
)

// AEMU is a AEMU target.
type AEMU struct {
	QEMU
}

var _ FuchsiaTarget = (*AEMU)(nil)

// NewAEMU returns a new AEMU target with a given configuration.
func NewAEMU(ctx context.Context, config QEMUConfig, opts Options) (*AEMU, error) {
	target, err := NewQEMU(ctx, config, opts)
	if err != nil {
		return nil, err
	}

	target.binary = aemuBinaryName
	target.builder = qemu.NewAEMUCommandBuilder()
	target.isQEMU = false

	return &AEMU{QEMU: *target}, nil
}
