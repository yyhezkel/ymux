package insights

import (
	"strings"
	"testing"
)

func TestDockerHint(t *testing.T) {
	for _, reason := range []string{"permission", "not_installed", "no_socket", "api_error"} {
		if dockerHint(reason) == "" {
			t.Fatalf("expected a hint for reason %q", reason)
		}
	}
	if dockerHint("") != "" {
		t.Fatal("ok state should have no hint")
	}
	if !strings.Contains(dockerHint("permission"), "docker") {
		t.Fatal("permission hint should mention the docker group")
	}
}

func TestDockerCandidatesHonorsDockerHost(t *testing.T) {
	t.Setenv("DOCKER_HOST", "unix:///custom/docker.sock")
	c := dockerCandidates()
	if len(c) != 1 || c[0] != "/custom/docker.sock" {
		t.Fatalf("DOCKER_HOST not honored: %+v", c)
	}
}

func TestDockerCandidatesIncludesRootlessAndStandard(t *testing.T) {
	t.Setenv("DOCKER_HOST", "")
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	c := dockerCandidates()
	joined := strings.Join(c, "|")
	for _, want := range []string{"/run/user/1000/docker.sock", "/var/run/docker.sock", "/run/docker.sock"} {
		if !strings.Contains(joined, want) {
			t.Fatalf("candidate %q missing from %v", want, c)
		}
	}
}

func TestCPUPctFromDelta(t *testing.T) {
	// 100ns of container time over 1000ns of system time on 4 cpus = 40%.
	got := cpuPctFromDelta(cpuPrev{total: 1000, system: 10000}, cpuPrev{total: 1100, system: 11000}, 4)
	if got != 40 {
		t.Fatalf("want 40, got %v", got)
	}
	// online_cpus unreported → assume 1.
	if got := cpuPctFromDelta(cpuPrev{}, cpuPrev{total: 50, system: 100}, 0); got != 50 {
		t.Fatalf("want 50, got %v", got)
	}
	// Counters reset (restart) or no system progress → 0, never negative/Inf.
	if got := cpuPctFromDelta(cpuPrev{total: 500, system: 5000}, cpuPrev{total: 10, system: 100}, 2); got != 0 {
		t.Fatalf("reset should be 0, got %v", got)
	}
	if got := cpuPctFromDelta(cpuPrev{total: 1, system: 100}, cpuPrev{total: 2, system: 100}, 2); got != 0 {
		t.Fatalf("zero system delta should be 0, got %v", got)
	}
}

func TestEvictCPUPrev(t *testing.T) {
	dockerCPUPrev.Store("keep", cpuPrev{1, 2})
	dockerCPUPrev.Store("gone", cpuPrev{3, 4})
	evictCPUPrev([]dockerListEntry{{Id: "keep"}})
	if _, ok := dockerCPUPrev.Load("keep"); !ok {
		t.Fatal("listed container baseline was evicted")
	}
	if _, ok := dockerCPUPrev.Load("gone"); ok {
		t.Fatal("unlisted container baseline survived")
	}
	dockerCPUPrev.Delete("keep")
}
