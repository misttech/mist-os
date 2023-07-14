// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package artifactory

import (
	"archive/tar"
	"fmt"
	"log"
	"os"
	"path"
	"path/filepath"

	"go.fuchsia.dev/fuchsia/tools/build"
)

const (
	// uefiImageName is the canonical name of an x64 UEFI image in the
	// manifest.
	uefiImageName = "uefi-disk"
	// gceUploadName is the canonical name of the uploaded GCE image.
	gceUploadName = "disk.tar.gz"
	// gceImageName is the canonical expected name of a source image in GCE.
	gceImageName = "disk.raw"
	// elfSizesName is the canonical expected name of ELF sizes JSON file.
	elfSizesName = "elf_sizes.json"
)

// ImageUploads parses the image manifest located in the build and returns a
// list of Uploads for the images used for testing.
func ImageUploads(mods *build.Modules, namespace string) ([]Upload, error) {
	return imageUploads(mods, namespace)
}

func imageUploads(mods imgModules, namespace string) ([]Upload, error) {
	manifestName := filepath.Base(mods.ImageManifest())

	files := []Upload{
		{
			Source:      mods.ImageManifest(),
			Destination: path.Join(namespace, manifestName),
			Signed:      true,
		},
	}

	// The same image might appear in multiple entries.
	seen := make(map[string]struct{})
	var elfSizesPath string
	for _, img := range mods.Images() {
		// Skip uploading product_bundle as they are uploaded by ProductBundleUpload.
		if img.Name == productBundleName && img.Type == productBundleType {
			continue
		}

		if _, ok := seen[img.Path]; ok {
			continue
		}
		seen[img.Path] = struct{}{}

		switch img.Name {
		case elfSizesName:
			if elfSizesPath != "" {
				return nil, fmt.Errorf("found multiple elf_sizes.json, this is unexpected, fix this by including only one elf_sizes.json target in the build graph: %s, %s", elfSizesPath, img.Path)
			}
			elfSizesPath = img.Path
			// Upload elf_sizes.json to the root of images directory, so it's easily
			// accessible in GCS.
			files = append(files, Upload{
				Source:      filepath.Join(mods.BuildDir(), elfSizesPath),
				Destination: path.Join(namespace, elfSizesName),
			})
		case uefiImageName:
			srcPath := filepath.Join(mods.BuildDir(), img.Path)
			info, err := os.Stat(srcPath)
			if err != nil {
				log.Printf("failed to stat gce image on disk: %s", err)
				continue
			}
			dest := filepath.Join(filepath.Dir(img.Path), gceUploadName)
			files = append(files, Upload{
				Source:      srcPath,
				Destination: path.Join(namespace, dest),
				Compress:    true,
				Signed:      true,
				TarHeader: &tar.Header{
					Format: tar.FormatGNU,
					Name:   gceImageName,
					Mode:   0666,
					Size:   info.Size(),
				},
			})
		default:
			files = append(files, Upload{
				Source:      filepath.Join(mods.BuildDir(), img.Path),
				Destination: path.Join(namespace, img.Path),
				Compress:    true,
				// Images should be signed for release builds.
				Signed: true,
			})
		}
	}
	return files, nil
}

type imgModules interface {
	BuildDir() string
	Images() []build.Image
	ImageManifest() string
}
