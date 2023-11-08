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
	blobs, err := u.SystemImageBlobs(ctx)
	if err != nil {
		return 0, err
	}

	return u.p.repo.sumBlobBlockSizes(ctx, blobs)
}

// SystemImageBlobs returns the transitive blobs in the system image and the
// available set package blobs contained in the system image.
func (u *SystemImagePackage) SystemImageBlobs(ctx context.Context) (map[build.MerkleRoot]struct{}, error) {
	visitedPackages := make(map[build.MerkleRoot]struct{})

	// This will contain all the blobs in the system image packages, and
	// any of its subpackages.
	blobs, err := u.p.TransitiveBlobs(ctx)
	if err != nil {
		return nil, err
	}

	for path, merkle := range u.packages {
		pkg, err := newPackage(ctx, u.p.repo, path, merkle)
		if err != nil {
			return nil, err
		}

		if err := pkg.transitiveBlobs(ctx, visitedPackages, blobs); err != nil {
			return nil, err
		}
	}

	return blobs, nil
}

// AddRandomFiles will extend the system image package such that the total size
// of the system image package blobs and the available set package blobs is less
// than or equal to `maxSize`.
func (u *SystemImagePackage) AddRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	dstSystemImagePath string,
	maxSize uint64,
) (*SystemImagePackage, error) {
	initialBlobs, err := u.SystemImageBlobs(ctx)
	if err != nil {
		return nil, err
	}

	return u.EditPackage(ctx, func(p Package) (Package, error) {
		return p.addRandomFiles(
			ctx,
			rand,
			dstSystemImagePath,
			maxSize,
			initialBlobs,
		)
	})
}
