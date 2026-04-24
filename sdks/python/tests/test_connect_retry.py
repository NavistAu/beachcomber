import socket
import threading
import time
from pathlib import Path

import pytest


def test_connect_retries_succeed_after_brief_outage(tmp_path):
    from libbeachcomber.client import _connect_with_retry

    sock_path = tmp_path / "sock"

    def binder():
        time.sleep(0.4)
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.bind(str(sock_path))
        s.listen(1)
        s.settimeout(5)
        try:
            conn, _ = s.accept()
            conn.close()
        except Exception:
            pass
        s.close()

    threading.Thread(target=binder, daemon=True).start()

    start = time.time()
    sock = _connect_with_retry(str(sock_path))
    elapsed = time.time() - start

    assert sock is not None
    assert elapsed >= 0.25, f"should have retried; elapsed={elapsed}"
    sock.close()


def test_connect_retries_exhaust(tmp_path):
    from libbeachcomber.client import _connect_with_retry

    sock_path = tmp_path / "nosock"
    start = time.time()
    with pytest.raises((ConnectionRefusedError, FileNotFoundError)):
        _connect_with_retry(str(sock_path))
    elapsed = time.time() - start
    # 250 + 500 + 1000 = 1750ms minimum.
    assert elapsed >= 1.7, f"should wait through all retries; elapsed={elapsed}"
