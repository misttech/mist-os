// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/avb"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/zbi"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type UpdatePackage struct {
	r        *Repository
	p        Package
	packages map[string]build.MerkleRoot
}

func newUpdatePackage(ctx context.Context, r *Repository, p Package) (*UpdatePackage, error) {
	f, err := p.Open(ctx, "packages.json")
	if err != nil {
		return nil, err
	}
	defer f.Close()

	packages, err := util.ParsePackagesJSON(f)
	if err != nil {
		return nil, err
	}

	return &UpdatePackage{
		r:        r,
		p:        p,
		packages: packages,
	}, nil
}

func (u *UpdatePackage) Path() string {
	return u.p.Path()
}

func (u *UpdatePackage) Merkle() build.MerkleRoot {
	return u.p.Merkle()
}

func (u *UpdatePackage) OpenPackage(ctx context.Context, path string) (Package, error) {
	merkle, ok := u.packages[path]
	if !ok {
		return Package{}, fmt.Errorf("could not find %s merkle in update package %s", path, u.p.Path())
	}

	return newPackage(ctx, u.r, path, merkle)
}

// Extract the update package `srcUpdatePackage` into a temporary directory,
// then build and publish it to the repository as the `dstUpdatePackage` name.
func (u *UpdatePackage) EditUpdatePackage(
	ctx context.Context,
	dstUpdatePackage string,
	editFunc func(path string) error,
) (*UpdatePackage, error) {
	p, err := u.r.EditPackage(ctx, u.p, dstUpdatePackage, func(tempDir string) error {
		return editFunc(tempDir)
	})
	if err != nil {
		return nil, err
	}

	return newUpdatePackage(ctx, u.r, p)
}

// RehostUpdatePackage will rewrite the `packages.json` file to use `repoName`
// path, to avoid collisions with the `fuchsia.com` repository name.
func (u *UpdatePackage) RehostUpdatePackage(
	ctx context.Context,
	dstUpdatePath string,
	repoName string,
) (*UpdatePackage, error) {
	return u.EditUpdatePackage(ctx, dstUpdatePath, func(tempDir string) error {
		packagesJsonPath := filepath.Join(tempDir, "packages.json")
		logger.Infof(ctx, "setting host name in %q to %q", packagesJsonPath, repoName)

		err := util.AtomicallyWriteFile(packagesJsonPath, 0600, func(f *os.File) error {
			src, err := os.Open(packagesJsonPath)
			if err != nil {
				return fmt.Errorf("failed to open packages.json %q: %w", packagesJsonPath, err)
			}
			defer src.Close()

			if err := util.RehostPackagesJSON(bufio.NewReader(src), bufio.NewWriter(f), repoName); err != nil {
				return fmt.Errorf("failed to rehost package.json: %w", err)
			}

			return nil
		})

		if err != nil {
			return fmt.Errorf("failed to atomically overwrite %q: %w", packagesJsonPath, err)
		}

		return nil
	})
}

func (u *UpdatePackage) EditUpdatePackageWithNewSystemImage(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	repoName string,
	systemImage Package,
	dstUpdatePackagePath string,
	bootfsCompression string,
	editFunc func(path string) error,
) (*UpdatePackage, error) {
	return u.EditUpdatePackage(
		ctx,
		dstUpdatePackagePath,
		func(tempDir string) error {
			if err := zbiTool.UpdateZBIWithNewSystemImageMerkle(ctx,
				systemImage.Merkle(),
				tempDir,
				bootfsCompression,
			); err != nil {
				return err
			}

			pathToZbi := filepath.Join(tempDir, "zbi")
			vbmetaPath := filepath.Join(tempDir, "fuchsia.vbmeta")
			if err := avbTool.MakeVBMetaImageWithZbi(ctx, vbmetaPath, vbmetaPath, pathToZbi); err != nil {
				return err
			}

			packagesJsonPath := filepath.Join(tempDir, "packages.json")
			err := util.AtomicallyWriteFile(packagesJsonPath, 0600, func(f *os.File) error {
				src, err := os.Open(packagesJsonPath)
				if err != nil {
					return fmt.Errorf("failed to open packages.json %q: %w", packagesJsonPath, err)
				}
				defer src.Close()

				if err := util.UpdateHashValuePackagesJSON(
					bufio.NewReader(src),
					bufio.NewWriter(f),
					repoName,
					"system_image/0",
					systemImage.Merkle(),
				); err != nil {
					return fmt.Errorf("failed to update system_image_merkle in package.json: %w", err)
				}

				return nil
			})

			if err != nil {
				return fmt.Errorf("failed to atomically overwrite %q: %w", packagesJsonPath, err)
			}

			return editFunc(tempDir)
		},
	)
}

// Extracts the update package into a temporary directory, and injects the
// specified vbmeta property files into the vbmeta.
func (u *UpdatePackage) EditUpdatePackageWithVBMetaProperties(
	ctx context.Context,
	avbTool *avb.AVBTool,
	dstUpdatePath string,
	vbmetaPropertyFiles map[string]string,
	editFunc func(path string) error,
) (*UpdatePackage, error) {
	return u.EditUpdatePackage(
		ctx,
		dstUpdatePath,
		func(tempDir string) error {
			if err := editFunc(tempDir); err != nil {
				return err
			}

			srcVbmetaPath := filepath.Join(tempDir, "fuchsia.vbmeta")
			if _, err := os.Stat(srcVbmetaPath); err != nil {
				return fmt.Errorf("vbmeta %q does not exist in repo: %w", srcVbmetaPath, err)
			}

			logger.Infof(ctx, "updating vbmeta %q", srcVbmetaPath)

			err := util.AtomicallyWriteFile(srcVbmetaPath, 0600, func(f *os.File) error {
				if err := avbTool.MakeVBMetaImage(ctx, f.Name(), srcVbmetaPath, vbmetaPropertyFiles); err != nil {
					return fmt.Errorf("failed to update vbmeta: %w", err)
				}
				return nil
			})

			if err != nil {
				return fmt.Errorf("failed to atomically overwrite %q: %w", srcVbmetaPath, err)
			}

			return nil
		},
	)
}

func (u *UpdatePackage) OtaSize(ctx context.Context) (int64, error) {
	blobs := u.p.Blobs()

	// Next, find all the packages in the update package.
	packageMerkles := []build.MerkleRoot{}
	for _, merkle := range u.packages {
		packageMerkles = append(packageMerkles, merkle)
	}

	// Walk through all the packages and subpackages and gather up all the blobs.
	visitedPackages := make(map[build.MerkleRoot]struct{})
	for {
		if len(packageMerkles) == 0 {
			break
		}

		// Pop a merkle from the list.
		index := len(packageMerkles) - 1
		merkle := packageMerkles[index]
		packageMerkles = packageMerkles[:index]

		// Skip if we've already processed this merkle.
		if _, ok := visitedPackages[merkle]; ok {
			continue
		}
		visitedPackages[merkle] = struct{}{}

		// Open up each package and add its blobs to our set.
		p, err := newPackage(ctx, u.r, "", merkle)
		if err != nil {
			return 0, err
		}

		for blob := range p.Blobs() {
			blobs[blob] = struct{}{}
		}

		// Push any subpackages onto our stack.
		for _, subpackageMerkle := range p.Subpackages() {
			packageMerkles = append(packageMerkles, subpackageMerkle)
		}
	}

	// Finally sum up all the blob sizes from the blob store.
	totalSize := int64(0)
	for blob := range blobs {
		size, err := u.r.BlobSize(ctx, blob)
		if err != nil {
			return 0, nil
		}

		totalSize += size
	}

	return totalSize, nil
}
