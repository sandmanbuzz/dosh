"""Acceptance tests boot the real EFI artifact in QEMU; see tools/doshlab.py."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools"))

import doshlab  # noqa: E402


@pytest.fixture(params=["tcg", "kvm"])
def accel(request):
    """Every guest test runs on both execution paths.

    A skipped kvm test means native guest execution was NOT demonstrated on
    this machine; the tcg result alone only shows the supervisor's logic
    against an emulated processor.
    """
    if request.param == "kvm" and (reason := doshlab.kvm_unavailable()):
        pytest.skip(f"native guest execution NOT exercised: {reason}")
    return request.param


@pytest.fixture
def dosh(accel):
    """Boot DOSH with the given capsules on the current execution path."""

    def boot(*runs, **kwargs):
        kwargs.setdefault("timeout", 60 if accel == "kvm" else 180)
        result = doshlab.run(list(runs), accel=accel, **kwargs)
        assert not result.timed_out, f"DOSH did not power off in time\n{result.log}"
        # Booting and enabling virtualization are separate facts from any
        # guest having run; each test asserts on capsule events for that.
        result.one("boot", stage="supervisor")
        result.one("hv", backend="svm", status="enabled")
        return result

    return boot
