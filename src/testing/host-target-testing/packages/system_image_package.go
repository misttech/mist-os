// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"context"
	"math/rand"

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
	return u.EditPackage(ctx, func(p Package) (Package, error) {
		return p.EditContents(ctx, dstSystemImagePath, editFunc)
	})
}

func (u *SystemImagePackage) EditPackage(
	ctx context.Context,
	editFunc func(p Package) (Package, error),
) (*SystemImagePackage, error) {
	p, err := editFunc(u.p)
	if err != nil {
		return nil, err
	}

	return &SystemImagePackage{
		p:        p,
		packages: u.packages,
	}, nil
}

// SystemImageSize returns the transitive space needed to store all the blobs
// in the system image. It does not include the update image package
// blobs, since those are garbage collected during the OTA.
func (u *SystemImagePackage) SystemImageSize(ctx context.Context) (uint64, error) {
	visitedPackages := make(map[build.MerkleRoot]struct{})

	// This will contain all the blobs in the system image packages, and
	// any of its subpackages.
	blobs := u.p.Blobs()

	for path, merkle := range u.packages {
		pkg, err := newPackage(ctx, u.p.repo, path, merkle)
		if err != nil {
			return 0, err
		}

		if err := pkg.transitiveBlobs(ctx, visitedPackages, blobs); err != nil {
			return 0, err
		}
	}

	return u.p.repo.sumBlobSizes(ctx, blobs)
}

func (u *SystemImagePackage) AddRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	dstSystemImagePath string,
	maxSize uint64,
) (*SystemImagePackage, error) {
	initialSize, err := u.SystemImageSize(ctx)
	if err != nil {
		return nil, err
	}

	return u.EditPackage(ctx, func(p Package) (Package, error) {
		return u.p.addRandomFiles(ctx, rand, dstSystemImagePath, initialSize, maxSize)
	})
}
