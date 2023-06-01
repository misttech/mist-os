// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package file

import (
	"bytes"
	"crypto/sha1"
	"encoding/base64"
	"fmt"
	"hash/fnv"
	"strings"

	classifierLib "github.com/google/licenseclassifier/v2"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/file/notice"
)

// FileData holds the text information (and some metadata) for a given file.
//
// Many NOTICE files will include several license texts in it.
// FileData represents one of those segments. It also maintains a line number
// that points to the location of this license text in the original NOTICE file,
// making it easier to find this license text again later.
type FileData struct {
	file        *File
	libraryName string
	lineNumber  int
	data        []byte

	searchResults *classifierLib.Results

	// ---------------
	licenseType string
	patternPath string
	url         string

	beingSurfaced      string
	sourceCodeIncluded string

	// SPDX fields for referencing this file content
	// in the SPDX output file.
	spdxName string
	spdxID   string

	hash string
}

// Order implements sort.Interface for []*FileData based on the FilePath field.
type OrderFileData []*FileData

func (a OrderFileData) Len() int      { return len(a) }
func (a OrderFileData) Swap(i, j int) { a[i], a[j] = a[j], a[i] }
func (a OrderFileData) Less(i, j int) bool {
	if a[i].file.absPath < a[j].file.absPath {
		return true
	}
	if a[i].file.absPath > a[j].file.absPath {
		return false
	}
	return a[i].lineNumber < a[j].lineNumber
}

func LoadFileData(f *File, content []byte) ([]*FileData, error) {
	data := make([]*FileData, 0)

	// The "LicenseFormat" field of each file is set at the project level
	// (in README.fuchsia files) and it affects how they are analyzed here.
	switch f.fileType {

	// File.LicenseFormat == RegularFile
	// All source files belonging to "The Fuchsia Authors" (fuchsia.git)
	// must contain Copyright header information.
	// Source files in other projects must not have restrictive license types.
	case RegularFile:
		fallthrough

	// File.LicenseFormat == SingleLicense
	// Regular LICENSE files that contain text for a single license.
	case SingleLicense:
		data = append(data, &FileData{
			file:        f,
			lineNumber:  0,
			libraryName: f.projectName,
			data:        bytes.TrimSpace(content),
			url:         f.url,
		})

	// File.LicenseFormat == MultiLicense*
	// NOTICE files that contain text for multiple licenses.
	// See the files in the /notice subdirectory for more info.
	case MultiLicense:
		ndata, err := notice.ParseOneDelimiter(f.absPath, content)
		if err != nil {
			return nil, err
		}
		for _, d := range ndata {
			data = append(data, &FileData{
				file:        f,
				lineNumber:  d.LineNumber,
				libraryName: d.LibraryName,
				data:        bytes.TrimSpace(d.LicenseText),
			})
		}

	case MultiLicenseChromium:
		ndata, err := notice.ParseChromium(f.absPath, content)
		if err != nil {
			return nil, err
		}
		for _, d := range ndata {
			data = append(data, &FileData{
				file:        f,
				lineNumber:  d.LineNumber,
				libraryName: d.LibraryName,
				data:        bytes.TrimSpace(d.LicenseText),
			})
		}
	case MultiLicenseFlutter:
		ndata, err := notice.ParseFlutter(f.absPath, content)
		if err != nil {
			return nil, err
		}
		for _, d := range ndata {
			data = append(data, &FileData{
				file:        f,
				lineNumber:  d.LineNumber,
				libraryName: d.LibraryName,
				data:        bytes.TrimSpace(d.LicenseText),
			})
		}
	case MultiLicenseAndroid:
		ndata, err := notice.ParseAndroid(f.absPath, content)
		if err != nil {
			return nil, err
		}
		for _, d := range ndata {
			data = append(data, &FileData{
				file:        f,
				lineNumber:  d.LineNumber,
				libraryName: d.LibraryName,
				data:        bytes.TrimSpace(d.LicenseText),
			})
		}
	case MultiLicenseGoogle:
		ndata, err := notice.ParseGoogle(f.absPath, content)
		if err != nil {
			return nil, err
		}
		for _, d := range ndata {
			data = append(data, &FileData{
				file:        f,
				lineNumber:  d.LineNumber,
				libraryName: d.LibraryName,
				data:        bytes.TrimSpace(d.LicenseText),
			})
		}

	default:
		return nil, fmt.Errorf("File type %v is unknown for filedata processing.", f.fileType)
	}

	for _, d := range data {
		// Some characters in license texts are not interpreted properly
		// (mismatched encodings?) and end up as garbled characters in output files.
		// We replace those characters with properly encoded ones here.
		for _, r := range Config.Replacements {
			d.data = bytes.ReplaceAll(d.data, []byte(r.Replace), []byte(r.With))
		}

		if d.libraryName == "" {
			d.libraryName = f.projectName
		}

		d.spdxName = fmt.Sprintf("%s", d.libraryName)

		h := fnv.New128a()
		h.Write([]byte(fmt.Sprintf("%s %s %s", d.libraryName, d.file.relPath, string(d.data))))
		d.spdxID = fmt.Sprintf("LicenseRef-filedata-%x", h.Sum([]byte{}))
	}
	return data, nil
}

func (fd *FileData) Search() {
	if fd.searchResults == nil {
		results := classifier.Match(fd.data)
		fd.searchResults = &results
	}
}

// Setters
// TODO(fxbug.dev/125736): Remove all setters.
func (fd *FileData) SetLicenseType(lt string) { fd.licenseType = lt }

// Getters
func (fd *FileData) File() *File                           { return fd.file }
func (fd *FileData) LibraryName() string                   { return fd.libraryName }
func (fd *FileData) LineNumber() int                       { return fd.lineNumber }
func (fd *FileData) Data() []byte                          { return fd.data }
func (fd *FileData) LicenseType() string                   { return fd.licenseType }
func (fd *FileData) PatternPath() string                   { return fd.patternPath }
func (fd *FileData) URL() string                           { return fd.url }
func (fd *FileData) BeingSurfaced() string                 { return fd.beingSurfaced }
func (fd *FileData) SourceCodeIncluded() string            { return fd.sourceCodeIncluded }
func (fd *FileData) SPDXName() string                      { return fd.spdxName }
func (fd *FileData) SPDXID() string                        { return fd.spdxID }
func (fd *FileData) SearchResults() *classifierLib.Results { return fd.searchResults }

// For copyright data, we want "filedata" to only contain the copyright
// text. Not the rest of the source code in the given file.
// This method lets us set the filedata data after detecting the copyright
// header info.
func (fd *FileData) SetData(data []byte) {
	fd.data = data
	fd.hash = ""
	fd.Hash()
}

// Use the config replacement / filedataurls information, along with
// the project name and URL (if it exists) to define the actual location
// of the license file on the internet.
func (fd *FileData) UpdateURLs(projectName string, projectURL string) {
	if strings.Contains(fd.file.relPath, "prebuilt") {
		for _, ur := range Config.FileDataURLs {
			if _, ok := ur.Projects[projectName]; !ok {
				continue
			}

			prefix := ur.Prefix
			if url, ok := ur.Replacements[fd.libraryName]; ok {
				fd.url = fmt.Sprintf("%v%v", prefix, url)
				return
			}
		}
	}
}

// Hash the content of this filedata object, to help detect duplicate texts
// and help reduce the final NOTICE filesize.
func (fd *FileData) Hash() string {
	if len(fd.hash) > 0 {
		return fd.hash
	}

	hasher := sha1.New()
	hasher.Write(bytes.TrimSpace(fd.data))
	fd.hash = base64.URLEncoding.EncodeToString(hasher.Sum(nil))
	return fd.hash
}
