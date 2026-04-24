package beachcomber

import (
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestConnectRetriesSucceedAfterBriefOutage(t *testing.T) {
	dir, _ := os.MkdirTemp("", "comb-retry-")
	defer os.RemoveAll(dir)
	sockPath := filepath.Join(dir, "sock")

	go func() {
		time.Sleep(400 * time.Millisecond)
		l, err := net.Listen("unix", sockPath)
		if err != nil {
			return
		}
		defer l.Close()
		conn, _ := l.Accept()
		if conn != nil {
			conn.Close()
		}
	}()

	start := time.Now()
	conn, err := connectWithRetry(sockPath, 5*time.Second)
	elapsed := time.Since(start)

	if err != nil {
		t.Fatalf("connectWithRetry: %v", err)
	}
	defer conn.Close()

	if elapsed < 250*time.Millisecond {
		t.Fatalf("should have retried; elapsed=%v", elapsed)
	}
}

func TestConnectRetriesExhaust(t *testing.T) {
	dir, _ := os.MkdirTemp("", "comb-retry-")
	defer os.RemoveAll(dir)
	sockPath := filepath.Join(dir, "nosock")

	start := time.Now()
	_, err := connectWithRetry(sockPath, 100*time.Millisecond)
	elapsed := time.Since(start)

	if err == nil {
		t.Fatal("expected error")
	}
	if elapsed < 1700*time.Millisecond {
		t.Fatalf("should have waited through all retries; elapsed=%v", elapsed)
	}
}
