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
	"os"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/src/sys/pkg/bin/pm/build"
	far "go.fuchsia.dev/fuchsia/src/sys/pkg/lib/far/go"
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
