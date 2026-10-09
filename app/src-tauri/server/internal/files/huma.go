package files

// huma.go — the Files API (/api/v2/files/*) as typed huma operations. huma
// reflects request/response structs into the server's OpenAPI (Phase 77 S4), so
// the spec can no longer drift from the handlers. The wire contract is byte-for
// -byte the same as the S2 stdlib handlers: same query params, status codes,
// headers (X-Ymux-Truncated, Content-Disposition), and JSON shapes.
//
// Binary responses (read/download) use huma.StreamResponse so we own the body
// writer directly — huma's format registry only marshals JSON/CBOR, and these
// endpoints emit raw application/octet-stream bytes.

import (
	"context"
	"errors"
	"io"
	"net/http"
	"path"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/danielgtaylor/huma/v2"

	"ymux-server/internal/core"
)

// secured is the bearer requirement stamped on every Files operation; the api
// package's middleware enforces it (ops with no Security are public).
var secured = []map[string][]string{{"bearerAuth": {}}}

// FileListBody mirrors the S2 {cwd, entries} response.
type FileListBody struct {
	Cwd     string           `json:"cwd"`
	Entries []core.FileEntry `json:"entries"`
}

// UploadResultBody mirrors the S2 {path, size, sha256} response.
type UploadResultBody struct {
	Path   string `json:"path"`
	Size   int64  `json:"size"`
	Sha256 string `json:"sha256"`
}

// okBody is the {ok:true} acknowledgement shared by mutating endpoints.
type okBody struct {
	OK bool `json:"ok"`
}

// httpErr maps a provider error to the huma status error with the same code the
// S2 stdlib `fail` helper produced.
func httpErr(err error) error {
	switch {
	case err == nil:
		return nil
	case errors.Is(err, ErrOutsideSandbox):
		return huma.Error403Forbidden(err.Error())
	case errors.Is(err, ErrNotFound):
		return huma.Error404NotFound(err.Error())
	case errors.Is(err, ErrIsDir):
		return huma.Error400BadRequest(err.Error())
	case errors.Is(err, ErrExists):
		return huma.Error409Conflict(err.Error())
	default:
		return huma.Error500InternalServerError(err.Error())
	}
}

// RegisterHuma mounts the Files operations onto a shared huma API. The api
// package calls this on the server-wide API (so all subsystems share one spec).
func (s *Service) RegisterHuma(api huma.API) {
	s.registerMutations(api) // Phase 116 (F2)
	huma.Register(api, huma.Operation{
		OperationID: "files-list", Method: http.MethodGet, Path: "/api/v2/files/list",
		Summary: "List a directory (sandboxed to the server's files root)",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct {
		Path  string `query:"path"`
		Depth int    `query:"depth"`
	}) (*struct{ Body FileListBody }, error) {
		depth := in.Depth
		if depth == 0 {
			depth = 1
		}
		cwd, entries, err := s.fp.List(in.Path, depth)
		if err != nil {
			return nil, httpErr(err)
		}
		if entries == nil {
			entries = []core.FileEntry{}
		}
		return &struct{ Body FileListBody }{Body: FileListBody{Cwd: cwd, Entries: entries}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "files-read", Method: http.MethodGet, Path: "/api/v2/files/read",
		Summary: "Read up to max_bytes of a file (raw bytes; X-Ymux-Truncated header)",
		Tags:    []string{"files"}, Security: secured,
		Responses: octetResponses(),
	}, func(_ context.Context, in *struct {
		Path     string `query:"path"`
		MaxBytes int64  `query:"max_bytes"`
	}) (*huma.StreamResponse, error) {
		data, truncated, err := s.fp.Read(in.Path, in.MaxBytes)
		if err != nil {
			return nil, httpErr(err)
		}
		return &huma.StreamResponse{Body: func(ctx huma.Context) {
			ctx.SetHeader("Content-Type", "application/octet-stream")
			if truncated {
				ctx.SetHeader("X-Ymux-Truncated", "true")
			}
			_, _ = ctx.BodyWriter().Write(data)
		}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "files-upload", Method: http.MethodPost, Path: "/api/v2/files/upload",
		Summary: "Upload a file (multipart form; ?path= destination)",
		Tags:    []string{"files"}, Security: secured,
		MaxBodyBytes: DefaultMaxUpload + (1 << 20),
	}, func(_ context.Context, in *struct {
		Path    string `query:"path" required:"true"`
		RawBody huma.MultipartFormFiles[struct {
			File huma.FormFile `form:"file" required:"true"`
		}]
	}) (*struct{ Body UploadResultBody }, error) {
		f := in.RawBody.Data().File
		if !f.IsSet {
			return nil, huma.Error400BadRequest("missing 'file' part")
		}
		data, err := io.ReadAll(f)
		if err != nil {
			return nil, huma.Error400BadRequest("read upload: " + err.Error())
		}
		sum, size, err := s.fp.Write(in.Path, data)
		if err != nil {
			return nil, httpErr(err)
		}
		return &struct{ Body UploadResultBody }{Body: UploadResultBody{Path: in.Path, Size: size, Sha256: sum}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "files-download", Method: http.MethodGet, Path: "/api/v2/files/download",
		Summary: "Download a file (octet-stream, attachment)",
		Tags:    []string{"files"}, Security: secured,
		Responses: octetResponses(),
	}, func(_ context.Context, in *struct {
		Path string `query:"path" required:"true"`
	}) (*huma.StreamResponse, error) {
		rc, size, err := s.fp.Open(in.Path)
		if err != nil {
			return nil, httpErr(err)
		}
		name := path.Base(in.Path)
		return &huma.StreamResponse{Body: func(ctx huma.Context) {
			defer rc.Close()
			ctx.SetHeader("Content-Type", "application/octet-stream")
			ctx.SetHeader("Content-Length", strconv.FormatInt(size, 10))
			ctx.SetHeader("Content-Disposition", "attachment; filename=\""+name+"\"")
			_, _ = io.Copy(ctx.BodyWriter(), rc)
		}}, nil
	})

	huma.Register(api, huma.Operation{
		OperationID: "files-delete", Method: http.MethodDelete, Path: "/api/v2/files/delete",
		Summary: "Delete a file or empty directory (recursive=true: a whole directory)",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct {
		Path      string `query:"path" required:"true"`
		Recursive bool   `query:"recursive" doc:"Phase 116: remove a directory with everything under it"`
	}) (*struct{ Body okBody }, error) {
		del := s.fp.Delete
		if in.Recursive {
			del = s.fp.DeleteTree
		}
		if err := del(in.Path); err != nil {
			return nil, httpErr(err)
		}
		return &struct{ Body okBody }{Body: okBody{OK: true}}, nil
	})
}

// ── Phase 116 (WEB-DESIGN F2): mkdir, rename, copy, archive, unzip ──────

// PathBody is a single sandbox path.
type PathBody struct {
	Path string `json:"path" minLength:"1"`
}

// MoveBody is a source and a destination inside the sandbox.
type MoveBody struct {
	From string `json:"from" minLength:"1"`
	To   string `json:"to" minLength:"1"`
}

// ArchiveBody packs names (relative to cwd) into cwd/output.
type ArchiveBody struct {
	Cwd    string   `json:"cwd"`
	Names  []string `json:"names" minItems:"1"`
	Output string   `json:"output" minLength:"1"`
	Format string   `json:"format" enum:"zip,targz"`
}

// registerMutations mounts the F2 operations (called from RegisterHuma).
func (s *Service) registerMutations(api huma.API) {
	huma.Register(api, huma.Operation{
		OperationID: "files-mkdir", Method: http.MethodPost, Path: "/api/v2/files/mkdir",
		Summary: "Create a directory (no parents; 409 if it exists)",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct{ Body PathBody }) (*struct{ Body okBody }, error) {
		if err := s.fp.Mkdir(in.Body.Path); err != nil {
			return nil, httpErr(err)
		}
		return &struct{ Body okBody }{Body: okBody{OK: true}}, nil
	})
	huma.Register(api, huma.Operation{
		OperationID: "files-rename", Method: http.MethodPost, Path: "/api/v2/files/rename",
		Summary: "Rename / move inside the sandbox (never overwrites)",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct{ Body MoveBody }) (*struct{ Body okBody }, error) {
		if err := s.fp.Rename(in.Body.From, in.Body.To); err != nil {
			return nil, httpErr(err)
		}
		return &struct{ Body okBody }{Body: okBody{OK: true}}, nil
	})
	huma.Register(api, huma.Operation{
		OperationID: "files-copy", Method: http.MethodPost, Path: "/api/v2/files/copy",
		Summary: "Copy a file or a directory (recursive; never overwrites)",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct{ Body MoveBody }) (*struct{ Body okBody }, error) {
		if err := s.fp.Copy(in.Body.From, in.Body.To); err != nil {
			return nil, httpErr(err)
		}
		return &struct{ Body okBody }{Body: okBody{OK: true}}, nil
	})
	huma.Register(api, huma.Operation{
		OperationID: "files-archive", Method: http.MethodPost, Path: "/api/v2/files/archive",
		Summary: "Pack items into a .zip or .tar.gz next to them",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct{ Body ArchiveBody }) (*struct{ Body PathBody }, error) {
		out, err := s.fp.Archive(in.Body.Cwd, in.Body.Names, in.Body.Output, in.Body.Format)
		if err != nil {
			return nil, archiveErr(err)
		}
		return &struct{ Body PathBody }{Body: PathBody{Path: s.rel(out)}}, nil
	})
	huma.Register(api, huma.Operation{
		OperationID: "files-unzip", Method: http.MethodPost, Path: "/api/v2/files/unzip",
		Summary: "Extract a .zip into <dir>/<name>/",
		Tags:    []string{"files"}, Security: secured,
	}, func(_ context.Context, in *struct{ Body PathBody }) (*struct{ Body PathBody }, error) {
		dest, err := s.fp.Unzip(in.Body.Path)
		if err != nil {
			return nil, archiveErr(err)
		}
		return &struct{ Body PathBody }{Body: PathBody{Path: s.rel(dest)}}, nil
	})
}

// archiveErr: the sandbox errors keep their codes; a tool's own failure
// ("zip failed (exit 127): …") is a 422 carrying that message, which the UI
// reads to offer tar.
func archiveErr(err error) error {
	if errors.Is(err, ErrOutsideSandbox) || errors.Is(err, ErrNotFound) || errors.Is(err, ErrIsDir) || errors.Is(err, ErrExists) {
		return httpErr(err)
	}
	return huma.Error422UnprocessableEntity(err.Error())
}

// rel turns an absolute sandbox path back into the client's root-relative one.
func (s *Service) rel(abs string) string {
	root := s.fp.Root()
	if r, err := filepath.Rel(root, abs); err == nil && !strings.HasPrefix(r, "..") {
		return "/" + filepath.ToSlash(r)
	}
	return abs
}

// octetResponses documents a raw binary 200 so generated SDKs treat read +
// download as byte streams rather than JSON.
func octetResponses() map[string]*huma.Response {
	return map[string]*huma.Response{
		"200": {
			Description: "raw file bytes",
			Content: map[string]*huma.MediaType{
				"application/octet-stream": {Schema: &huma.Schema{Type: huma.TypeString, Format: "binary"}},
			},
		},
	}
}
