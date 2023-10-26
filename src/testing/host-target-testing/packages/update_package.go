// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"bufio"
	"context"
	"fmt"
	"io"
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
	useNewUpdateFormat bool,
) (*UpdatePackage, error) {
	return u.EditUpdatePackage(
		ctx,
		dstUpdatePackagePath,
		func(tempDir string) error {
			if err := editUpdatePackageWithNewSystemImage(
				ctx,
				avbTool,
				zbiTool,
				u.r,
				repoName,
				&systemImage,
				map[string]string{},
				dstUpdatePackagePath,
				bootfsCompression,
				tempDir,
				useNewUpdateFormat,
			); err != nil {
				return err
			}

			return nil
		},
	)
}

func editUpdatePackageWithNewSystemImage(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	repo *Repository,
	repoName string,
	systemImage *Package,
	vbmetaPropertyFiles map[string]string,
	dstUpdatePackagePath string,
	bootfsCompression string,
	tempDir string,
	useNewUpdateFormat bool,
) error {
	hasImages := false
	imagesPath := filepath.Join(tempDir, "images.json")
	images, err := util.ParseImagesJSON(imagesPath)
	if err == nil {
		hasImages = true
	}

	if hasImages {
		if err := editExternalUpdatePackageWithNewSystemImage(
			ctx,
			avbTool,
			zbiTool,
			repo,
			systemImage,
			vbmetaPropertyFiles,
			dstUpdatePackagePath,
			bootfsCompression,
			tempDir,
			images,
		); err != nil {
			return err
		}
	} else if useNewUpdateFormat {
		imagesPath := filepath.Join(tempDir, "images.json.orig")
		images, err := util.ParseImagesJSON(imagesPath)
		if err != nil {
			return err
		}

		if err := editExternalUpdatePackageWithNewSystemImage(
			ctx,
			avbTool,
			zbiTool,
			repo,
			systemImage,
			vbmetaPropertyFiles,
			dstUpdatePackagePath,
			bootfsCompression,
			tempDir,
			images,
		); err != nil {
			return err
		}

		if err := os.Remove(filepath.Join(tempDir, "zbi")); err != nil {
			return err
		}

		if err := os.Remove(filepath.Join(tempDir, "fuchsia.vbmeta")); err != nil {
			return err
		}
	} else {
		if err := editInlinedUpdatePackageWithNewSystemImage(
			ctx,
			avbTool,
			zbiTool,
			repo,
			repoName,
			systemImage,
			vbmetaPropertyFiles,
			dstUpdatePackagePath,
			bootfsCompression,
			tempDir,
		); err != nil {
			return err
		}
	}

	if systemImage != nil {
		// Update `packages.json` to point at the new system image merkle.
		packagesJsonPath := filepath.Join(tempDir, "packages.json")
		if err := util.AtomicallyWriteFile(packagesJsonPath, 0600, func(f *os.File) error {
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
		}); err != nil {
			return fmt.Errorf("failed to atomically overwrite %q: %w", packagesJsonPath, err)
		}
	}

	return nil
}

func editExternalUpdatePackageWithNewSystemImage(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	repo *Repository,
	systemImage *Package,
	vbmetaPropertyFiles map[string]string,
	dstUpdatePackagePath string,
	bootfsCompression string,
	tempDir string,
	images util.ImagesManifest,
) error {
	dstUpdatePackageParts := strings.Split(dstUpdatePackagePath, "/")
	dstUpdatePackageName := dstUpdatePackageParts[0]

	// Create a new zbi+vbmeta package that points at the system image package.
	srcZbiUrl, srcZbiMerkle, err := images.GetPartition("fuchsia", "zbi")
	if err != nil {
		return err
	}

	if srcZbiUrl.Fragment == "" {
		return fmt.Errorf("url must have fragment for zbi: %q", srcZbiUrl)
	}

	srcVbmetaUrl, srcVbmetaMerkle, err := images.GetPartition("fuchsia", "vbmeta")
	if err != nil {
		return err
	}

	if srcVbmetaUrl.Fragment == "" {
		return fmt.Errorf("url must have fragment for vbmeta: %q", srcVbmetaUrl)
	}

	// The zbi and vbmeta package must be the same, since generating the vbmeta
	// for a zbi modifies the zbi.
	if srcZbiMerkle != srcVbmetaMerkle {
		return fmt.Errorf("zbi and vbmeta package must be the same: %s != %s", srcZbiUrl, srcVbmetaUrl)
	}

	srcZbiPackage, err := newPackage(ctx, repo, srcZbiUrl.Path[1:], srcZbiMerkle)
	if err != nil {
		return fmt.Errorf("could not parse package %s: %w", srcZbiUrl, err)
	}

	// Create a new zbi+vbmeta package
	dstZbiPackagePath := fmt.Sprintf("%s_images_fuchsia", dstUpdatePackageName)
	dstZbiPackage, err := repo.EditPackage(
		ctx,
		srcZbiPackage,
		dstZbiPackagePath,
		func(zbiTempDir string) error {
			// Inject the new system image merkle into the zbi.
			zbiPath := filepath.Join(zbiTempDir, srcZbiUrl.Fragment)
			vbmetaPath := filepath.Join(zbiTempDir, srcVbmetaUrl.Fragment)

			if systemImage != nil {
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
			}

			if len(vbmetaPropertyFiles) != 0 {
				logger.Infof(ctx, "updating vbmeta %q", vbmetaPath)

				if err := util.AtomicallyWriteFile(vbmetaPath, 0600, func(f *os.File) error {
					if err := avbTool.MakeVBMetaImage(ctx, f.Name(), vbmetaPath, vbmetaPropertyFiles); err != nil {
						return fmt.Errorf("failed to update vbmeta: %w", err)
					}
					return nil
				}); err != nil {
					return fmt.Errorf("failed to atomically overwrite %q: %w", vbmetaPath, err)
				}
			}

			return nil
		},
	)
	if err != nil {
		return err
	}

	// Update the zbi partition entry.
	dstZbiUrl := fmt.Sprintf(
		"fuchsia-pkg://%s/%s?hash=%s#%s",
		srcZbiUrl.Host,
		dstZbiPackage.Path(),
		dstZbiPackage.Merkle(),
		srcZbiUrl.Fragment,
	)
	logger.Infof(ctx, "updating update image zbi package url to %s", dstZbiUrl)

	zbiBlobPath, err := dstZbiPackage.FilePath(ctx, srcZbiUrl.Fragment)
	if err != nil {
		return err
	}

	if err := images.SetPartition("fuchsia", "zbi", dstZbiUrl, zbiBlobPath); err != nil {
		return err
	}

	// Update the vbmeta partition entry.
	dstVbmetaUrl := fmt.Sprintf(
		"fuchsia-pkg://%s/%s?hash=%s#%s",
		srcVbmetaUrl.Host,
		dstZbiPackage.Path(),
		dstZbiPackage.Merkle(),
		srcVbmetaUrl.Fragment,
	)
	logger.Infof(ctx, "updating update image vbmeta package url to %s", dstVbmetaUrl)

	vbmetaBlobPath, err := dstZbiPackage.FilePath(ctx, srcVbmetaUrl.Fragment)
	if err != nil {
		return err
	}

	if err := images.SetPartition("fuchsia", "vbmeta", dstVbmetaUrl, vbmetaBlobPath); err != nil {
		return err
	}

	imagesPath := filepath.Join(tempDir, "images.json")
	if err := util.UpdateImagesJSON(imagesPath, images); err != nil {
		return err
	}

	return nil
}

func editInlinedUpdatePackageWithNewSystemImage(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	repo *Repository,
	repoName string,
	systemImage *Package,
	vbmetaPropertyFiles map[string]string,
	dstUpdatePackagePath string,
	bootfsCompression string,
	tempDir string,
) error {
	vbmetaPath := filepath.Join(tempDir, "fuchsia.vbmeta")

	if systemImage != nil {
		zbiPath := filepath.Join(tempDir, "zbi")
		if err := zbiTool.UpdateZBIWithNewSystemImageMerkle(
			ctx,
			systemImage.Merkle(),
			zbiPath,
			zbiPath,
			bootfsCompression,
		); err != nil {
			return err
		}

		if err := avbTool.MakeVBMetaImageWithZbi(ctx, vbmetaPath, vbmetaPath, zbiPath); err != nil {
			return err
		}

		packagesJsonPath := filepath.Join(tempDir, "packages.json")
		if err := util.AtomicallyWriteFile(packagesJsonPath, 0600, func(f *os.File) error {
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
		}); err != nil {
			return fmt.Errorf("failed to atomically overwrite %q: %w", packagesJsonPath, err)
		}
	}

	if len(vbmetaPropertyFiles) != 0 {
		if _, err := os.Stat(vbmetaPath); err != nil {
			return fmt.Errorf("vbmeta %q does not exist in repo: %w", vbmetaPath, err)
		}

		logger.Infof(ctx, "updating vbmeta %q", vbmetaPath)

		if err := util.AtomicallyWriteFile(vbmetaPath, 0600, func(f *os.File) error {
			if err := avbTool.MakeVBMetaImage(ctx, f.Name(), vbmetaPath, vbmetaPropertyFiles); err != nil {
				return fmt.Errorf("failed to update vbmeta: %w", err)
			}
			return nil
		}); err != nil {
			return fmt.Errorf("failed to atomically overwrite %q: %w", vbmetaPath, err)
		}
	}

	return nil
}

func copyFile(srcPath string, dstPath string) error {
	src, err := os.Open(srcPath)
	if err != nil {
		return err
	}
	defer src.Close()

	dst, err := os.Open(dstPath)
	if err != nil {
		return err
	}
	defer dst.Close()

	if _, err := io.Copy(src, dst); err != nil {
		return err
	}

	return nil
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

			if err := editUpdatePackageWithNewSystemImage(
				ctx,
				avbTool,
				nil,
				u.r,
				"fuchsia.com",
				nil,
				vbmetaPropertyFiles,
				dstUpdatePath,
				"",
				tempDir,
				false,
			); err != nil {
				return err
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
