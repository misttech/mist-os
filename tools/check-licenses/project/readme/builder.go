// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package readme

import (
	"fmt"
	"path/filepath"
	"strings"
)

// This builder struct and methods will generate a text blob that
// represents a README.fuchsia file.
type builder struct {
	name    string
	url     string
	version string

	licenses []*builderLicense

	// These variables are used when defining the above variables.
	path        string
	dir         string
	parent      string
	grandparent string
}

type builderLicense struct {
	file   string
	url    string
	format string
}

func (b *builder) setName(name string)       { b.name = name }
func (b *builder) setURL(url string)         { b.url = url }
func (b *builder) setVersion(version string) { b.version = version }
func (b *builder) setPath(path string) {
	b.path = path
	b.dir = filepath.Base(path)
	b.parent = filepath.Base(filepath.Dir(path))
	b.grandparent = filepath.Base(filepath.Dir(filepath.Dir(path)))
}

func (b *builder) addLicense(file, url, format string) {
	l := &builderLicense{
		file:   file,
		url:    url,
		format: format,
	}
	b.licenses = append(b.licenses, l)
}

func (b *builder) build() string {
	var sb strings.Builder
	sb.WriteString(fmt.Sprintf("Name: %s\n", b.name))
	sb.WriteString(fmt.Sprintf("URL: %s\n", b.url))

	if b.version != "" {
		sb.WriteString(fmt.Sprintf("Version: %s\n", b.version))
	}

	for _, l := range b.licenses {
		sb.WriteString(fmt.Sprintf("License File: %s\n", l.file))
		sb.WriteString(fmt.Sprintf("License File URL: %s\n", l.url))
		sb.WriteString(fmt.Sprintf("License File Format: %s\n", l.format))
	}
	return sb.String()
}
