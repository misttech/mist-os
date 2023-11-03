// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/avb"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/zbi"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type UpdatePackage struct {
	r                 *Repository
	p                 Package
	packages          map[string]build.MerkleRoot
	hasImagesManifest bool
	images            util.ImagesManifest
}

func newUpdatePackage(ctx context.Context, r *Repository, p Package) (*UpdatePackage, error) {
	// Parse the images manifest, if it exists.
	hasImagesManifest := false
	var images util.ImagesManifest
	if f, err := p.Open(ctx, "images.json"); err == nil {
		defer f.Close()

		i, err := util.ParseImagesJSON(f)
		if err != nil {
			return nil, err
		}

		images = i
		hasImagesManifest = true
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}

	// Parse the packages list.
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
		r:                 r,
		p:                 p,
		packages:          packages,
		hasImagesManifest: hasImagesManifest,
		images:            images,
	}, nil
}

func (u *UpdatePackage) Path() string {
	return u.p.Path()
}

func (u *UpdatePackage) Merkle() build.MerkleRoot {
	return u.p.Merkle()
}

func (u *UpdatePackage) HasImagesManifest() bool {
	return u.hasImagesManifest
}

func (u *UpdatePackage) OpenPackage(ctx context.Context, path string) (Package, error) {
	merkle, ok := u.packages[path]
	if !ok {
		return Package{}, fmt.Errorf("could not find %s merkle in update package %s", path, u.p.Path())
	}

	return newPackage(ctx, u.r, path, merkle)
}

func (u *UpdatePackage) OpenSystemImagePackage(ctx context.Context) (*SystemImagePackage, error) {
	p, err := u.OpenPackage(ctx, "system_image/0")
	if err != nil {
		return nil, err
	}

	return &SystemImagePackage{
		p:        p,
		packages: u.packages,
	}, nil
}

func (u *UpdatePackage) OpenUpdateImages(ctx context.Context) (*UpdateImages, error) {
	f, err := u.p.Open(ctx, "images.json")
	if err != nil {
		return nil, fmt.Errorf(
			"update package %s does not have an images.json",
			u.p.Path(),
		)
	}
	defer f.Close()

	images, err := util.ParseImagesJSON(f)
	if err != nil {
		return nil, err
	}

	return newUpdateImages(ctx, u.r, images)
}

// Extract the update package `srcUpdatePackage` into a temporary directory,
// then build and publish it to the repository as the `dstUpdatePackage` name.
func (u *UpdatePackage) EditContents(
	ctx context.Context,
	dstUpdatePackagePath string,
	editFunc func(tempDir string) error,
) (*UpdatePackage, error) {
	p, err := u.p.EditContents(ctx, dstUpdatePackagePath, editFunc)
	if err != nil {
		return nil, err
	}

	return newUpdatePackage(ctx, u.r, p)
}

// Extract the update package `srcUpdatePackage` into a temporary directory,
// then build and publish it to the repository as the `dstUpdatePackage` name.
func (u *UpdatePackage) EditPackage(
	ctx context.Context,
	dstUpdatePath string,
	editFunc func(pkg Package) (Package, error),
) (*UpdatePackage, error) {
	p, err := editFunc(u.p)
	if err != nil {
		return nil, err
	}

	return newUpdatePackage(ctx, u.r, p)
}

// EditSystemImage will extract the system image into a temporary directory,
// provide it to the `editFunc`, then create a new update package that uses it.
func (u *UpdatePackage) EditSystemImagePackage(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	repoName string,
	dstUpdatePackagePath string,
	bootfsCompression string,
	useNewUpdateFormat bool,
	editFunc func(systemImage *SystemImagePackage) (*SystemImagePackage, error),
) (*UpdatePackage, *SystemImagePackage, error) {
	srcSystemImage, err := u.OpenSystemImagePackage(ctx)
	if err != nil {
		return nil, nil, fmt.Errorf(
			"failed to open system_image/0 from %s update package: %w",
			u.p.Path(),
			err,
		)
	}

	dstSystemImage, err := editFunc(srcSystemImage)
	if err != nil {
		return nil, nil, err
	}

	dstUpdate, err := u.EditUpdatePackageWithNewSystemImage(
		ctx,
		avbTool,
		zbiTool,
		repoName,
		dstSystemImage,
		dstUpdatePackagePath,
		bootfsCompression,
		useNewUpdateFormat,
	)
	if err != nil {
		return nil, nil, err
	}

	return dstUpdate, dstSystemImage, nil
}

// EditImagesPackage will extract the system image into a temporary directory,
// provide it to the `editFunc`, then create a new update package that uses it.
func (u *UpdatePackage) EditUpdateImages(
	ctx context.Context,
	dstUpdatePackagePath string,
	editFunc func(updateImages *UpdateImages) (*UpdateImages, error),
) (*UpdatePackage, *UpdateImages, error) {
	srcUpdateImages, err := u.OpenUpdateImages(ctx)
	if err != nil {
		return nil, nil, err
	}

	dstUpdateImages, err := editFunc(srcUpdateImages)
	if err != nil {
		return nil, nil, err
	}

	dstUpdate, err := u.EditContents(ctx, dstUpdatePackagePath, func(tempDir string) error {
		imagesPath := filepath.Join(tempDir, "images.json")
		f, err := os.Open(imagesPath)
		if err != nil {
			return err
		}
		defer f.Close()

		if err := util.UpdateImagesJSON(imagesPath, dstUpdateImages.images); err != nil {
			return err
		}

		return nil
	})
	if err != nil {
		return nil, nil, err
	}

	return dstUpdate, dstUpdateImages, nil
}

// RehostUpdatePackage will rewrite the `packages.json` file to use `repoName`
// path, to avoid collisions with the `fuchsia.com` repository name.
func (u *UpdatePackage) RehostUpdatePackage(
	ctx context.Context,
	dstUpdatePath string,
	repoName string,
) (*UpdatePackage, error) {
	return u.EditContents(ctx, dstUpdatePath, func(tempDir string) error {
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
	systemImage *SystemImagePackage,
	dstUpdatePackagePath string,
	bootfsCompression string,
	useNewUpdateFormat bool,
) (*UpdatePackage, error) {
	return u.EditContents(
		ctx,
		dstUpdatePackagePath,
		func(tempDir string) error {
			// Update the update package's zbi and vbmeta to point at this system image.
			if err := u.editZbiAndVbmeta(
				ctx,
				dstUpdatePackagePath,
				useNewUpdateFormat,
				tempDir,
				func(zbiPath string, vbmetaPath string) error {
					if err := zbiTool.UpdateZBIWithNewSystemImageMerkle(
						ctx,
						systemImage.Merkle(),
						zbiPath,
						zbiPath,
						bootfsCompression,
					); err != nil {
						return err
					}

					if err := avbTool.MakeVBMetaImageWithZbi(
						ctx,
						vbmetaPath,
						vbmetaPath,
						zbiPath,
					); err != nil {
						return err
					}

					return nil
				},
			); err != nil {
				return err
			}

			// Update the `system_image/0` entry for this new system image.
			packagesJsonPath := filepath.Join(tempDir, "packages.json")
			return util.UpdateHashValuePackagesJSON(
				packagesJsonPath,
				repoName,
				"system_image/0",
				systemImage.Merkle(),
			)
		},
	)
}

// Extracts the update package into a temporary directory, and injects the
// specified vbmeta property files into the vbmeta.
func (u *UpdatePackage) EditUpdatePackageWithVBMetaProperties(
	ctx context.Context,
	avbTool *avb.AVBTool,
	dstUpdatePackagePath string,
	vbmetaPropertyFiles map[string]string,
	useNewUpdateFormat bool,
	editFunc func(path string) error,
) (*UpdatePackage, error) {
	return u.EditContents(
		ctx,
		dstUpdatePackagePath,
		func(tempDir string) error {
			if err := editFunc(tempDir); err != nil {
				return err
			}

			// Update the update package's zbi and vbmeta to point at this system image.
			if err := u.editZbiAndVbmeta(
				ctx,
				dstUpdatePackagePath,
				useNewUpdateFormat,
				tempDir,
				func(zbiPath string, vbmetaPath string) error {
					logger.Infof(ctx, "updating vbmeta %q", vbmetaPath)

					if err := util.AtomicallyWriteFile(vbmetaPath, 0600, func(f *os.File) error {
						if err := avbTool.MakeVBMetaImage(ctx, f.Name(), vbmetaPath, vbmetaPropertyFiles); err != nil {
							return fmt.Errorf("failed to update vbmeta: %w", err)
						}
						return nil
					}); err != nil {
						return fmt.Errorf("failed to atomically overwrite %q: %w", vbmetaPath, err)
					}

					return nil
				},
			); err != nil {
				return err
			}

			return nil
		},
	)
}

// editZbiAndVbmeta will allow the `editFunc` to modify the zbi and vbmeta from
// an update package, whether or not those files are in a side-package listed in
// the images.json file, or directly embedded in the update package.
func (u *UpdatePackage) editZbiAndVbmeta(
	ctx context.Context,
	dstUpdatePackagePath string,
	useNewUpdateFormat bool,
	tempDir string,
	editFunc func(zbiPath string, vbmetaPath string) error,
) error {
	if u.hasImagesManifest {
		updateImages, err := u.OpenUpdateImages(ctx)
		if err != nil {
			return err
		}

		if err := u.replaceUpdateImages(
			ctx,
			dstUpdatePackagePath,
			tempDir,
			updateImages,
			editFunc,
		); err != nil {
			return err
		}
	} else {
		zbiPath := filepath.Join(tempDir, "zbi")
		vbmetaPath := filepath.Join(tempDir, "fuchsia.vbmeta")

		if useNewUpdateFormat {
			imagesPath := filepath.Join(tempDir, "images.json.orig")
			f, err := os.Open(imagesPath)
			if err != nil {
				return err
			}
			defer f.Close()

			images, err := util.ParseImagesJSON(f)
			if err != nil {
				return err
			}

			updateImages, err := newUpdateImages(ctx, u.r, images)
			if err != nil {
				return err
			}

			if err := u.replaceUpdateImages(
				ctx,
				dstUpdatePackagePath,
				tempDir,
				updateImages,
				editFunc,
			); err != nil {
				return err
			}

			if err := os.Remove(zbiPath); err != nil {
				return err
			}

			if err := os.Remove(vbmetaPath); err != nil {
				return err
			}
		} else {
			if err := editFunc(zbiPath, vbmetaPath); err != nil {
				return err
			}
		}
	}

	return nil
}

func (u *UpdatePackage) replaceUpdateImages(
	ctx context.Context,
	dstUpdatePackagePath string,
	tempDir string,
	srcUpdateImages *UpdateImages,
	editFunc func(zbiPath string, vbmetaPath string) error,
) error {
	dstUpdatePackageParts := strings.Split(dstUpdatePackagePath, "/")
	dstUpdatePackageName := dstUpdatePackageParts[0]
	dstZbiPackagePath := fmt.Sprintf("%s_update_images_zbi", dstUpdatePackageName)

	dstUpdateImages, err := srcUpdateImages.EditZbiAndVbmeta(
		ctx,
		dstZbiPackagePath,
		editFunc,
	)
	if err != nil {
		return err
	}

	imagesPath := filepath.Join(tempDir, "images.json")
	if err := util.UpdateImagesJSON(imagesPath, dstUpdateImages.images); err != nil {
		return err
	}

	return nil
}

// OtaMaxNeededSize returns the maximum space needed to OTA this update package
// onto a device.
func (u *UpdatePackage) OtaMaxNeededSize(ctx context.Context) (int64, error) {
	// The system updater flow is currently:
	//
	// * Download the update package.
	// * If the update package contains an images manifest:
	//   * Download all the update image packages.
	//   * Install the update images.
	//   * GC the update image packages.
	// * Download the system image packages.
	//
	// Therefore, the maximum space we need available in blobfs to install
	// an OTA is `update + max(images, system image)`.

	updateImagesSize, err := u.UpdateAndImagesSize(ctx)
	if err != nil {
		return 0, err
	}

	systemImageSize, err := u.UpdateAndSystemImageSize(ctx)
	if err != nil {
		return 0, err
	}

	if systemImageSize < updateImagesSize {
		return updateImagesSize, nil
	} else {
		return systemImageSize, nil
	}
}

// UpdateImagesSize returns the transitive space needed to install the update
// package and all the blobs in update images.
func (u *UpdatePackage) UpdateAndImagesSize(ctx context.Context) (int64, error) {
	visitedPackages := make(map[build.MerkleRoot]struct{})

	// This will contain all the blobs in the update package and the update
	// image packages, and any of their subpackages.
	blobs := u.p.Blobs()

	if err := u.transitiveUpdateImagesBlobs(ctx, visitedPackages, blobs); err != nil {
		return 0, err
	}

	return u.totalBlobSize(ctx, blobs)
}

// SystemImageSize returns the transitive space needed to store the update
// package and all the blobs in the system image. It does not include the
// update image package blobs, since those are garbage collected during the
// OTA.
func (u *UpdatePackage) UpdateAndSystemImageSize(ctx context.Context) (int64, error) {
	visitedPackages := make(map[build.MerkleRoot]struct{})

	// This will contain all the blobs in the update package and the system
	// image packages, and any of their subpackages.
	blobs := u.p.Blobs()

	if err := u.transitiveSystemImageBlobs(ctx, visitedPackages, blobs); err != nil {
		return 0, err
	}

	return u.totalBlobSize(ctx, blobs)
}

func (u *UpdatePackage) transitiveUpdateImagesBlobs(
	ctx context.Context,
	visitedPackages map[build.MerkleRoot]struct{},
	blobs map[build.MerkleRoot]struct{},
) error {
	if f, err := u.p.Open(ctx, "images.json"); err == nil {
		defer f.Close()

		images, err := util.ParseImagesJSON(f)
		if err != nil {
			return err
		}

		for _, partition := range images.Contents.Partitions {
			_, merkle, err := util.ParsePackageUrl(partition.Url)
			if err != nil {
				return err
			}

			if err := u.transitivePackageBlobs(ctx, visitedPackages, blobs, merkle); err != nil {
				return err
			}
		}

		for _, firmware := range images.Contents.Firmware {
			_, merkle, err := util.ParsePackageUrl(firmware.Url)
			if err != nil {
				return err
			}

			if err := u.transitivePackageBlobs(ctx, visitedPackages, blobs, merkle); err != nil {
				return err
			}
		}

	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}

	return nil
}

func (u *UpdatePackage) transitiveSystemImageBlobs(
	ctx context.Context,
	visitedPackages map[build.MerkleRoot]struct{},
	blobs map[build.MerkleRoot]struct{},
) error {
	for _, merkle := range u.packages {
		if err := u.transitivePackageBlobs(ctx, visitedPackages, blobs, merkle); err != nil {
			return err
		}
	}

	return nil
}

func (u *UpdatePackage) transitivePackageBlobs(
	ctx context.Context,
	visitedPackages map[build.MerkleRoot]struct{},
	blobs map[build.MerkleRoot]struct{},
	merkle build.MerkleRoot,
) error {
	// Exit early if we've already processed this package.
	if _, ok := visitedPackages[merkle]; ok {
		return nil
	}
	visitedPackages[merkle] = struct{}{}

	// Open up each package and add its blobs to our set.
	p, err := newPackage(ctx, u.r, "", merkle)
	if err != nil {
		return err
	}

	for blob := range p.Blobs() {
		blobs[blob] = struct{}{}
	}

	// Recurse into any subpackages.
	for _, subpackageMerkle := range p.Subpackages() {
		if err := u.transitivePackageBlobs(ctx, visitedPackages, blobs, subpackageMerkle); err != nil {
			return err
		}
	}

	return nil
}

// totalBlobSize sums up all the blob sizes from the blob store.
func (u *UpdatePackage) totalBlobSize(
	ctx context.Context,
	blobs map[build.MerkleRoot]struct{},
) (int64, error) {
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
