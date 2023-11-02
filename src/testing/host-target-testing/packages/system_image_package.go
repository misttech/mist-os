// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"context"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
)

type SystemImagePackage struct {
	p        Package
	packages map[string]build.MerkleRoot
}

func (u *SystemImagePackage) Path() string {
	return u.p.Path()
}

func (u *SystemImagePackage) Merkle() build.MerkleRoot {
	return u.p.Merkle()
}

func (u *SystemImagePackage) EditContents(
	ctx context.Context,
	dstSystemImagePath string,
	editFunc func(tempDir string) error,
) (*SystemImagePackage, error) {
	p, err := u.p.EditContents(ctx, dstSystemImagePath, editFunc)
	if err != nil {
		return nil, err
	}

	return &SystemImagePackage{
		p:        p,
		packages: u.packages,
	}, nil
}
