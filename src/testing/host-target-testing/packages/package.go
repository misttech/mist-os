// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package packages

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"math/rand"
	"os"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
	far "go.fuchsia.dev/fuchsia/src/sys/pkg/lib/far/go"
	"go.fuchsia.dev/fuchsia/tools/lib/logger"
)

const (
	BlobBlockSize = 4096
)

type FileData []byte

type Package struct {
	merkle      build.MerkleRoot
	repo        *Repository
	path        string
	contents    build.MetaContents
	subpackages map[string]build.MerkleRoot
}

// newPackage extracts out a package from the repository.
func newPackage(
	ctx context.Context,
	repo *Repository,
	path string,
	merkle build.MerkleRoot,
) (Package, error) {
	// Need to parse out the package meta.far to find the package contents.
	blob, err := repo.OpenUncompressedBlob(ctx, merkle)
	if err != nil {
		return Package{}, err
	}
	defer blob.Close()

	f, err := far.NewReader(blob)
	if err != nil {
		return Package{}, err
	}
	defer f.Close()

	b, err := f.ReadFile("meta/contents")
	if err != nil {
		return Package{}, err
	}

	contents, err := build.ParseMetaContents(bytes.NewReader(b))
	if err != nil {
		return Package{}, err
	}

	subpackages := make(map[string]build.MerkleRoot)
	if b, err := f.ReadFile("meta/fuchsia.pkg/subpackages"); err == nil {
		var metaSubpackages *build.MetaSubpackages
		if err := json.Unmarshal(b, &metaSubpackages); err != nil {
			return Package{}, fmt.Errorf("Unable to parse subpackage for package %s: %w", merkle, err)
		}

		for path, merkleString := range metaSubpackages.Subpackages {
			merkle, err := build.DecodeMerkleRoot([]byte(merkleString))
			if err != nil {
				return Package{}, err
			}

			subpackages[path] = merkle
		}
	}

	return Package{
		merkle:      merkle,
		repo:        repo,
		path:        path,
		contents:    contents,
		subpackages: subpackages,
	}, nil
}

// Path returns the package path.
func (p *Package) Path() string {
	return p.path
}

// Merkle returns the meta.far merkle.
func (p *Package) Merkle() build.MerkleRoot {
	return p.merkle
}

// Returns the package's blobs.
func (p *Package) Blobs() map[build.MerkleRoot]struct{} {
	blobs := make(map[build.MerkleRoot]struct{})
	blobs[p.merkle] = struct{}{}

	for _, merkle := range p.contents {
		blobs[merkle] = struct{}{}
	}

	return blobs
}

// Returns the package's subpackages.
func (p *Package) Subpackages() map[string]build.MerkleRoot {
	subpackages := make(map[string]build.MerkleRoot)
	for path, merkle := range p.subpackages {
		subpackages[path] = merkle
	}

	return subpackages
}

// Open opens a file in the package.
func (p *Package) Open(ctx context.Context, path string) (*os.File, error) {
	merkle, ok := p.contents[path]
	if !ok {
		return nil, os.ErrNotExist
	}

	return p.repo.OpenUncompressedBlob(ctx, merkle)
}

func (p *Package) FilePath(ctx context.Context, path string) (string, error) {
	merkle, ok := p.contents[path]
	if !ok {
		return "", os.ErrNotExist
	}

	return p.repo.UncompressedBlobPath(ctx, merkle)
}

// ReadFile reads a file from a package.
func (p *Package) ReadFile(ctx context.Context, path string) ([]byte, error) {
	r, err := p.Open(ctx, path)
	if err != nil {
		return nil, err
	}

	return io.ReadAll(r)
}

func (p *Package) Expand(ctx context.Context, dir string) error {
	for path := range p.contents {
		data, err := p.ReadFile(ctx, path)
		if err != nil {
			return fmt.Errorf("invalid path. %w", err)
		}
		realPath := filepath.Join(dir, path)
		if err := os.MkdirAll(filepath.Dir(realPath), 0755); err != nil {
			return fmt.Errorf("could not create parent directories for %s, %w", realPath, err)
		}
		if err = os.WriteFile(realPath, data, 0644); err != nil {
			return fmt.Errorf("could not export %s to %s. %w", path, realPath, err)
		}
	}
	blob, err := p.repo.OpenUncompressedBlob(ctx, p.merkle)
	if err != nil {
		return fmt.Errorf("failed to open meta.far blob. %w", err)
	}
	defer blob.Close()

	f, err := far.NewReader(blob)
	if err != nil {
		return fmt.Errorf("failed to open reader on blob. %w", err)
	}
	defer f.Close()

	for _, path := range f.List() {
		data, err := f.ReadFile(path)
		if err != nil {
			fmt.Errorf("failed to read %s. %w", path, err)
		}
		realPath := filepath.Join(dir, path)
		if err := os.MkdirAll(filepath.Dir(realPath), 0755); err != nil {
			return fmt.Errorf("could not create parent directories for %s, %w", realPath, err)
		}
		if err = os.WriteFile(realPath, data, 0644); err != nil {
			fmt.Errorf("failed to write file %s. %w", realPath, err)
		}
	}

	return nil
}

func (p *Package) EditContents(
	ctx context.Context,
	dstPath string,
	editFunc func(tempDir string) error,
) (Package, error) {
	return p.repo.EditPackage(ctx, *p, dstPath, editFunc)
}

// TransitiveBlobs returns all the blobs in this package and subpackages.
func (p *Package) TransitiveBlobs(ctx context.Context) (map[build.MerkleRoot]struct{}, error) {
	visitedPackages := make(map[build.MerkleRoot]struct{})
	blobs := make(map[build.MerkleRoot]struct{})

	if err := p.transitiveBlobs(ctx, visitedPackages, blobs); err != nil {
		return nil, err
	}

	return blobs, nil
}

func (p *Package) transitiveBlobs(
	ctx context.Context,
	visitedPackages map[build.MerkleRoot]struct{},
	blobs map[build.MerkleRoot]struct{},
) error {
	// Exit early if we've already processed this package.
	if _, ok := visitedPackages[p.merkle]; ok {
		return nil
	}
	visitedPackages[p.merkle] = struct{}{}

	for blob := range p.Blobs() {
		blobs[blob] = struct{}{}
	}

	// Recurse into any subpackages.
	for path, merkle := range p.Subpackages() {
		pkg, err := newPackage(ctx, p.repo, path, merkle)
		if err != nil {
			return err
		}

		if err := pkg.transitiveBlobs(ctx, visitedPackages, blobs); err != nil {
			return err
		}
	}

	return nil
}

// TransitiveBlobSize computes total size of all the blobs in this package and subpackages.
func (p *Package) TransitiveBlobSize(ctx context.Context) (uint64, error) {
	blobs, err := p.TransitiveBlobs(ctx)
	if err != nil {
		return 0, err
	}

	return p.repo.sumBlobSizes(ctx, blobs)
}

// AddRandomFiles will add random files to this package that increases the
// package size up to `maxSize` bytes. Each file will be less than 10MiB.
func (p *Package) AddRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	dstPath string,
	maxSize uint64,
) (Package, error) {
	initialSize, err := p.TransitiveBlobSize(ctx)
	if err != nil {
		return Package{}, err
	}

	return p.addRandomFiles(ctx, rand, dstPath, initialSize, maxSize)
}

func (p *Package) addRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	dstPath string,
	initialSize uint64,
	maxSize uint64,
) (Package, error) {
	logger.Infof(
		ctx,
		"Trying to grow package %s with size %d to up to %d bytes",
		p.path,
		initialSize,
		maxSize,
	)

	if maxSize < initialSize {
		return Package{}, fmt.Errorf(
			"initial package %s with size %d is greater than the max size %d",
			p.path,
			initialSize,
			maxSize,
		)
	}

	bytesToAdd := maxSize - initialSize
	for bytesToAdd > 0 {
		dstPackage, err := p.repo.EditPackage(ctx, *p, dstPath, func(tempDir string) error {
			return generateRandomFiles(ctx, rand, tempDir, bytesToAdd)
		})
		if err != nil {
			return Package{}, fmt.Errorf(
				"failed to create the package %s: %w",
				dstPath,
				err,
			)
		}

		size, err := p.TransitiveBlobSize(ctx)
		if err != nil {
			return Package{}, err
		}

		if maxSize < size {
			logger.Warningf(
				ctx,
				"package %s with size %d is bigger than %d, trying again",
				dstPackage.Path(),
				size,
				maxSize,
			)

			// Shrink the package size by the amount we overshot by.
			bytesToAdd -= size - maxSize
		} else {
			logger.Infof(
				ctx,
				"Accepting package %s with size %d is smaller than %d",
				dstPackage.Path(),
				size,
				maxSize,
			)
			return dstPackage, nil
		}
	}

	return Package{}, fmt.Errorf(
		"failed to generate package %s with size less than %d",
		dstPath,
		maxSize,
	)
}

// GenerateRandomFiles will create a number of files that sum up to
// `bytesToAdd` random bytes, each which will be less than 10MiB.
func generateRandomFiles(
	ctx context.Context,
	rand *rand.Rand,
	tempDir string,
	bytesToAdd uint64,
) error {
	otaTestDir := filepath.Join(tempDir, "ota-test")
	if err := os.Mkdir(otaTestDir, 0700); err != nil {
		return fmt.Errorf("failed to create %s: %w", otaTestDir, err)
	}

	index := 0
	bytes := [BlobBlockSize]byte{}

	const maxBlobSize = BlobBlockSize * 1000

	for bytesToAdd > 0 {
		// Create blobs up to 4MiB.
		var blobSize uint64
		if maxBlobSize < bytesToAdd {
			blobSize = maxBlobSize
		} else {
			blobSize = bytesToAdd
		}
		bytesToAdd -= blobSize

		blobPath := filepath.Join(otaTestDir, fmt.Sprintf("ota-test-file-%06d", index))
		f, err := os.Create(blobPath)
		if err != nil {
			return fmt.Errorf("failed to create %s: %w", blobPath, err)
		}
		defer f.Close()

		for blobSize > 0 {
			var n uint64
			if blobSize < BlobBlockSize {
				n = blobSize
			} else {
				n = BlobBlockSize
			}
			blobSize -= n

			if _, err := rand.Read(bytes[:n]); err != nil {
				return fmt.Errorf("failed to read %d random bytes: %w", n, err)
			}

			if _, err := f.Write(bytes[:n]); err != nil {
				return fmt.Errorf("failed to write random bytes to %s: %w", blobPath, err)
			}
		}

		index += 1
	}

	return nil
}
