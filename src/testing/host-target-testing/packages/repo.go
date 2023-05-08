// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/avb"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/ffx"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/util"
	"go.fuchsia.dev/fuchsia/src/testing/host-target-testing/zbi"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

type BlobStore interface {
	Dir() string
	OpenBlob(ctx context.Context, merkle string) (*os.File, error)
}

type DirBlobStore struct {
	dir string
}

func NewDirBlobStore(dir string) BlobStore {
	return &DirBlobStore{dir}
}

func (fs *DirBlobStore) OpenBlob(ctx context.Context, merkle string) (*os.File, error) {
	return os.Open(filepath.Join(fs.dir, merkle))
}

func (fs *DirBlobStore) Dir() string {
	return fs.dir
}

type Repository struct {
	Dir string
	// BlobsDir should be a directory called `blobs` where all the blobs are.
	BlobStore        BlobStore
	ffx              *ffx.FFXTool
	deliveryBlobType *int
}

type signed struct {
	Signed targets `json:"signed"`
}

type targets struct {
	Targets map[string]targetFile `json:"targets"`
}

type targetFile struct {
	Custom custom `json:"custom"`
}

type custom struct {
	Merkle string `json:"merkle"`
}

// NewRepository parses the repository from the specified directory. It returns
// an error if the repository does not exist, or it contains malformed metadata.
func NewRepository(ctx context.Context, dir string, blobStore BlobStore, ffx *ffx.FFXTool, deliveryBlobType *int) (*Repository, error) {
	logger.Infof(ctx, "creating a repository for %q and %q", dir, blobStore.Dir())

	// The repository may have out of date metadata. This updates the repository to
	// the latest version so TUF won't complain about the data being old.
	if err := ffx.RepositoryPublish(ctx, dir, []string{}, "--refresh-root"); err != nil {
		return nil, err
	}

	return &Repository{
		Dir:              filepath.Join(dir, "repository"),
		BlobStore:        blobStore,
		ffx:              ffx,
		deliveryBlobType: deliveryBlobType,
	}, nil
}

// NewRepositoryFromTar extracts a repository from a tar.gz, and returns a
// Repository parsed from it. It returns an error if the repository does not
// exist, or contains malformed metadata.
func NewRepositoryFromTar(ctx context.Context, dst string, src string, ffx *ffx.FFXTool, deliveryBlobType *int) (*Repository, error) {
	if err := util.Untar(ctx, dst, src); err != nil {
		return nil, fmt.Errorf("failed to extract packages: %w", err)
	}

	return NewRepository(ctx, filepath.Join(dst, "amber-files"), NewDirBlobStore(filepath.Join(dst, "amber-files", "repository", "blobs")), ffx, deliveryBlobType)
}

// OpenPackage opens a package from the repository.
func (r *Repository) OpenPackage(ctx context.Context, path string) (Package, error) {
	// Parse the targets file so we can access packages locally.
	f, err := os.Open(filepath.Join(r.Dir, "targets.json"))
	if err != nil {
		return Package{}, err
	}
	defer f.Close()

	var s signed
	if err = json.NewDecoder(f).Decode(&s); err != nil {
		return Package{}, err
	}

	if target, ok := s.Signed.Targets[path]; ok {
		return newPackage(ctx, r, target.Custom.Merkle)
	}

	return Package{}, fmt.Errorf("could not find package: %q", path)

}

func (r *Repository) OpenBlob(ctx context.Context, merkle string) (*os.File, error) {
	return r.BlobStore.OpenBlob(ctx, merkle)
}

func (r *Repository) Serve(ctx context.Context, localHostname string, repoName string, repoPort int) (*Server, error) {
	return newServer(ctx, r.Dir, r.BlobStore, localHostname, repoName, repoPort)
}

func (r *Repository) LookupUpdateSystemImageMerkle(ctx context.Context) (string, error) {
	return r.lookupUpdateContentPackageMerkle(ctx, "update/0", "system_image/0")
}

func (r *Repository) LookupUpdatePrimeSystemImage2Merkle(ctx context.Context) (string, error) {
	return r.lookupUpdateContentPackageMerkle(ctx, "update_prime2/0", "system_image/0")
}

func (r *Repository) VerifyMatchesAnyUpdateSystemImageMerkle(ctx context.Context, merkle string) error {
	systemImageMerkle, err := r.LookupUpdateSystemImageMerkle(ctx)
	if err != nil {
		return err
	}
	if merkle == systemImageMerkle {
		return nil
	}

	systemPrimeImage2Merkle, err := r.LookupUpdatePrimeSystemImage2Merkle(ctx)
	if err != nil {
		return err
	}
	if merkle == systemPrimeImage2Merkle {
		return nil
	}

	return fmt.Errorf("expected device to be running a system image of %s or %s, got %s",
		systemImageMerkle, systemPrimeImage2Merkle, merkle)
}

func (r *Repository) lookupUpdateContentPackageMerkle(ctx context.Context, updatePackageName string, contentPackageName string) (string, error) {
	// Extract the "packages" file from the "update" package.
	p, err := r.OpenPackage(ctx, updatePackageName)
	if err != nil {
		return "", err
	}
	f, err := p.Open(ctx, "packages.json")
	if err != nil {
		return "", err
	}

	packages, err := util.ParsePackagesJSON(f)
	if err != nil {
		return "", err
	}

	merkle, ok := packages[contentPackageName]
	if !ok {
		return "", fmt.Errorf("could not find %s merkle", contentPackageName)
	}

	return merkle, nil
}

// CreatePackage creates a package in this repository named `packagePath` by:
// * creating a temporary directory
// * passing it to the `createFunc` closure. The closure then adds any necessary files.
// * creating a package from the directory contents.
// * publishing the package to the repository with the `packagePath` path.
func (r *Repository) CreatePackage(
	ctx context.Context,
	packagePath string,
	createFunc func(path string) error,
) (string, error) {
	logger.Infof(ctx, "creating package %q", packagePath)

	// Extract the package name from the path. The variant currently is optional, but if specified, must be "0".
	packageName, packageVariant, found := strings.Cut(packagePath, "/")
	if found && packageVariant != "0" {
		return "", fmt.Errorf("invalid package path found: %q", packagePath)
	}
	packageVariant = "0"

	// Create temp directory. The content of this directory will be included in the package.
	tempDir, err := os.MkdirTemp("", "")
	if err != nil {
		return "", fmt.Errorf("failed to create a temp directory: %w", err)
	}
	defer os.RemoveAll(tempDir)

	// Package content will be created by the user by leveraging the createFunc closure.
	if err := createFunc(tempDir); err != nil {
		return "", fmt.Errorf("failed to create content of the package: %w", err)
	}

	// Create package from the temp directory. The package builder doesn't use
	// the repository name, so it can be set as `testrepository.com`.
	pkgBuilder, err := NewPackageBuilderFromDir(tempDir, packageName, packageVariant, "testrepository.com")
	if err != nil {
		return "", fmt.Errorf("failed to parse the package from %q: %w", tempDir, err)
	}

	// Publish the package and ger the merkle of the package.
	_, pkgMerkle, err := pkgBuilder.Publish(ctx, r)
	if err != nil {
		return "", fmt.Errorf("failed to publish the package %q: %w", packagePath, err)
	}

	return pkgMerkle, nil
}

// EditPackage takes the content of the source package from srcPackagePath,
// copies the content to destination package at dstPackagePath and edits the
// content at destination with the help of editFunc closure.
func (r *Repository) EditPackage(
	ctx context.Context,
	srcPackagePath string,
	dstPackagePath string,
	editFunc func(path string) error,
) (Package, error) {
	logger.Infof(ctx, "editing package %q. will create %q", srcPackagePath, dstPackagePath)

	// First get the source package located at srcPackagePath
	pkg, err := r.OpenPackage(ctx, srcPackagePath)
	if err != nil {
		return Package{}, fmt.Errorf("failed to open the package %q: %w", srcPackagePath, err)
	}

	// Next create a destination package based on the content oft the source package.
	pkgMerkle, err := r.CreatePackage(ctx, dstPackagePath, func(tempDir string) error {
		if err := pkg.Expand(ctx, tempDir); err != nil {
			return fmt.Errorf("failed to expand the package to %s: %w", tempDir, err)
		}

		// User can edit the content and return it.
		return editFunc(tempDir)
	})

	if err != nil {
		return Package{}, fmt.Errorf("failed to create the package %q: %w", dstPackagePath, err)
	}

	// Get the newly edited package located at pkgMerkle and return it.
	pkg, err = newPackage(ctx, r, pkgMerkle)
	if err != nil {
		return Package{}, fmt.Errorf("failed to edit the package %q: %w", pkgMerkle, err)
	}

	return pkg, nil
}

// Extracts the update package into a temporary directory, and injects the
// specified vbmeta property files into the vbmeta.
func (r *Repository) EditUpdatePackageWithVBMetaProperties(
	ctx context.Context,
	avbTool *avb.AVBTool,
	srcUpdatePackage string,
	dstUpdatePackage string,
	repoName string,
	vbmetaPropertyFiles map[string]string,
	editFunc func(path string) error) (Package, error) {
	return r.EditPackage(ctx, srcUpdatePackage, dstUpdatePackage, func(tempDir string) error {
		if err := editFunc(tempDir); err != nil {
			return err
		}

		packagesJsonPath := filepath.Join(tempDir, "packages.json")
		logger.Infof(ctx, "setting host name in %q to %q", packagesJsonPath, repoName)

		err := util.AtomicallyWriteFile(packagesJsonPath, 0600, func(f *os.File) error {
			src, err := os.Open(packagesJsonPath)
			if err != nil {
				return fmt.Errorf("failed to open packages.json %q: %w", packagesJsonPath, err)
			}

			if err := util.RehostPackagesJSON(bufio.NewReader(src), bufio.NewWriter(f), repoName); err != nil {
				return fmt.Errorf("failed to rehost package.json: %w", err)
			}

			return nil
		})

		if err != nil {
			return fmt.Errorf("failed to atomically overwrite %q: %w", packagesJsonPath, err)
		}

		srcVbmetaPath := filepath.Join(tempDir, "fuchsia.vbmeta")
		if _, err := os.Stat(srcVbmetaPath); err != nil {
			return fmt.Errorf("vbmeta %q does not exist in repo: %w", srcVbmetaPath, err)
		}

		logger.Infof(ctx, "updating vbmeta %q", srcVbmetaPath)

		err = util.AtomicallyWriteFile(srcVbmetaPath, 0600, func(f *os.File) error {
			if err := avbTool.MakeVBMetaImage(ctx, f.Name(), srcVbmetaPath, vbmetaPropertyFiles); err != nil {
				return fmt.Errorf("failed to update vbmeta: %w", err)
			}
			return nil
		})

		if err != nil {
			return fmt.Errorf("failed to atomically overwrite %q: %w", srcVbmetaPath, err)
		}

		return nil
	})
}

// Extract the update package `srcUpdatePackage` into a temporary directory,
// then build and publish it to the repository as the `dstUpdatePackage` name.
// It will automatically rewrite the `packages.json` file to use `repoName`
// path, to avoid collisions with the `fuchsia.com` repository name.
func (r *Repository) EditUpdatePackage(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	srcUpdatePackage string,
	dstUpdatePackage string,
	repoName string,
	editFunc func(path string) error,
) (Package, error) {
	vbmetaPropertyFiles := map[string]string{}

	return r.EditUpdatePackageWithVBMetaProperties(
		ctx,
		avbTool,
		srcUpdatePackage,
		dstUpdatePackage,
		repoName,
		vbmetaPropertyFiles,
		func(path string) error {
			return editFunc(path)
		})
}

func (r *Repository) EditUpdatePackageWithNewSystemImageMerkle(
	ctx context.Context,
	avbTool *avb.AVBTool,
	zbiTool *zbi.ZBITool,
	systemImageMerkle string,
	srcUpdatePackagePath string,
	dstUpdatePackagePath string,
	bootfsCompression string,
	editFunc func(path string) error,
) (Package, error) {
	repoName := "fuchsia.com"

	return r.EditUpdatePackage(ctx,
		avbTool, zbiTool,
		srcUpdatePackagePath,
		dstUpdatePackagePath,
		repoName,
		func(tempDir string) error {
			if err := zbiTool.UpdateZBIWithNewSystemImageMerkle(ctx,
				systemImageMerkle,
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

				if err := util.UpdateHashValuePackagesJSON(
					bufio.NewReader(src),
					bufio.NewWriter(f),
					repoName,
					"system_image/0",
					systemImageMerkle,
				); err != nil {
					return fmt.Errorf("failed to update system_image_merkle in package.json: %w", err)
				}

				return nil
			})

			if err != nil {
				return fmt.Errorf("failed to atomically overwrite %q: %w", packagesJsonPath, err)
			}

			return editFunc(tempDir)
		})
}

func (r *Repository) Publish(ctx context.Context, packageManifestPath string) error {
	repoDir := filepath.Dir(r.Dir)

	extraArgs := []string{"--blob-repo-dir", r.BlobStore.Dir()}
	if r.deliveryBlobType != nil {
		extraArgs = append(extraArgs, "--delivery-blob-type", fmt.Sprint(*r.deliveryBlobType))
	}

	return r.ffx.RepositoryPublish(ctx, repoDir, []string{packageManifestPath}, extraArgs...)
}
