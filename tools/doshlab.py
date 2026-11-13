#!/usr/bin/env python3
"""Boot DOSH on virtual bare metal and report what happened.

Each run gets a private directory holding its own boot disk, firmware
variable store and logs, and talks to QEMU over inherited file descriptors
only. Runs share nothing, so any number can execute at once.

As a command (inside the lab):

    tools/doshlab.py HELLO.COM
    tools/doshlab.py --accel tcg "ARGS.COM one two" HELLOMZ.EXE
    tools/doshlab.py --attach ASK.COM

Each argument is one capsule: a program from build/guests and its arguments.
Standard input is forwarded to the first capsule's console: all at once when
the capsule's session opens, or live, as a terminal, with --attach.
"""

from __future__ import annotations

import argparse
import os
import re
import select
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EFI = ROOT / "target/x86_64-unknown-uefi/release/dosh.efi"
GUESTS = ROOT / "build/guests"
OVMF_CODE = Path("/usr/share/OVMF/OVMF_CODE_4M.fd")
OVMF_VARS = Path("/usr/share/OVMF/OVMF_VARS_4M.fd")

# The two execution paths. Booting DOSH proves the supervisor; only a
# capsule's own exit event proves guest execution, and only under "kvm" did
# the guest's instructions run on the physical processor.
ACCELS = {
    # Software CPU with emulated AMD SVM: runs anywhere, proves the logic.
    "tcg": ["-machine", "q35,accel=tcg", "-cpu", "max"],
    # Hardware virtualization, nested: DOSH uses the real SVM of the host CPU.
    "kvm": ["-machine", "q35,accel=kvm", "-cpu", "host"],
}
# Serial ports after COM1 (the supervisor log): one console per capsule.
CONSOLE_PORTS = 3


def kvm_unavailable() -> str | None:
    """Why the hardware path cannot be used here, or None if it can."""
    if not os.access("/dev/kvm", os.R_OK | os.W_OK):
        return "no usable /dev/kvm"
    if " svm " not in Path("/proc/cpuinfo").read_text():
        return "host processor has no AMD SVM (DOSH has no VT-x backend yet)"
    nested = Path("/sys/module/kvm_amd/parameters/nested")
    if not nested.exists() or nested.read_text().strip() not in ("1", "Y"):
        return "nested virtualization is disabled in kvm_amd"
    return None


@dataclass
class Result:
    log: str
    """Everything written to COM1, firmware chatter included."""
    events: list[dict[str, str]]
    """The supervisor's own log lines, parsed: {"subject": ..., key: value}."""
    consoles: list[bytes]
    """What each capsule wrote to its console endpoint."""
    seconds: float
    timed_out: bool = False
    capsules: dict[int, dict[str, str]] = field(default_factory=dict)
    """Per capsule id, its final event: exit (with code) or fault."""

    def screen(self, capsule: int = 0) -> list[str]:
        """The 25 rows a terminal attached to the capsule would show."""
        return render_screen(self.consoles[capsule])

    def find(self, subject: str, **match: str) -> list[dict[str, str]]:
        return [e for e in self.events if e["subject"] == subject and all(e.get(k) == v for k, v in match.items())]

    def one(self, subject: str, **match: str) -> dict[str, str]:
        found = self.find(subject, **match)
        assert len(found) == 1, f"expected one {subject} {match} event, got {found}\n{self.log}"
        return found[0]


def render_screen(stream: bytes, rows: int = 25, cols: int = 80) -> list[str]:
    """What an 80x25 terminal shows after `stream`: the subset of ANSI that
    DOSH sends (cursor positioning, clear, colours, cursor visibility)."""
    grid = [[" "] * cols for _ in range(rows)]
    row = col = 0
    text = stream.decode("utf-8", errors="replace")
    i = 0
    while i < len(text):
        c = text[i]
        i += 1
        if c == "\x1b" and text[i : i + 1] == "[":
            m = re.compile(r"\[([0-9;?]*)([@-~])").match(text, i)
            if not m:
                break
            params, final = m.groups()
            i = m.end()
            nums = [int(n) if n else 0 for n in params.lstrip("?").split(";")] if params else []
            if final == "H":
                row, col = (nums[0] - 1 if nums else 0), (nums[1] - 1 if len(nums) > 1 else 0)
            elif final == "J" and nums[:1] == [2]:
                grid = [[" "] * cols for _ in range(rows)]
        elif c == "\r":
            col = 0
        elif c == "\n":
            row = min(row + 1, rows - 1)
        elif c >= " " and 0 <= row < rows and 0 <= col < cols:
            grid[row][col] = c
            col += 1
    return ["".join(r).rstrip() for r in grid]


def parse_events(log: str) -> list[dict[str, str]]:
    events = []
    for line in log.splitlines():
        _, mark, text = line.partition("[dosh] ")
        if mark:
            pairs = re.findall(r'(\w+)=(?:"([^"]*)"|(\S+))', text)
            events.append({"subject": text.split()[0], **{key: quoted or bare for key, quoted, bare in pairs}})
    return events


def session_banner(capsule: int) -> bytes:
    """What a serial endpoint prints when a capsule's console is attached."""
    return b"\r\n--- DOSH capsule %d ---\r\n" % capsule


def build_disk(image: Path, manifest_text: str, files: dict[str, bytes]) -> None:
    """A FAT boot disk: the supervisor, the boot manifest, and drive C:."""
    stage = image.parent / "c"
    stage.mkdir()
    for name, data in files.items():
        (stage / name).write_bytes(data)
    manifest = image.parent / "BOOT.CFG"
    manifest.write_text(manifest_text)

    def mtools(tool: str, *args: str) -> None:
        subprocess.run([tool, "-i", str(image), *args], check=True, env={**os.environ, "MTOOLS_SKIP_CHECK": "1"})

    with open(image, "wb") as f:
        f.truncate(48 << 20)
    mtools("mformat", "-F", "::")
    mtools("mmd", "::/EFI", "::/EFI/BOOT", "::/DOSH", "::/DOSH/C")
    mtools("mcopy", str(EFI), "::/EFI/BOOT/BOOTX64.EFI")
    mtools("mcopy", str(manifest), "::/DOSH/BOOT.CFG")
    for path in stage.iterdir():
        mtools("mcopy", str(path), f"::/DOSH/C/{path.name}")


def read_text(path: Path) -> str:
    return path.read_text(errors="replace") if path.exists() else ""


def inject_nmi(monitor: Path) -> None:
    """Raise a non-maskable interrupt through QEMU's monitor."""
    with socket.socket(socket.AF_UNIX) as sock:
        sock.connect(str(monitor))
        sock.sendall(b"nmi\n")
        sock.settimeout(1)
        try:
            sock.recv(4096)
        except TimeoutError:
            pass


def run(
    runs: list[str],
    *,
    accel: str = "tcg",
    stdin: list[bytes] | None = None,
    files: dict[str, bytes] | None = None,
    cpu: str | None = None,
    manifest: str | None = None,
    attach: bool = False,
    nmi: bool = False,
    timeout: float = 120,
    keep: Path | None = None,
) -> Result:
    """Boot DOSH with one capsule per entry of `runs` and wait for it to
    power off.

    `stdin[i]` is sent to capsule i's console. `files` adds to drive C:,
    which always holds the programs named in `runs`. `cpu` overrides the
    CPU model; `manifest` replaces the generated boot manifest verbatim.
    `attach` connects capsule 0's console to this process's own terminal
    instead of capturing it (firmware output included, as on a real line).
    `nmi` injects one non-maskable interrupt once the first capsule has
    started. `keep` preserves the run directory there for inspection.
    """
    files = dict(files or {})
    for line in runs:
        program = line.split()[0]
        if (GUESTS / program).exists():
            files.setdefault(program, (GUESTS / program).read_bytes())

    with tempfile.TemporaryDirectory(prefix="dosh-run-") as tmp:
        workdir = Path(tmp)
        if manifest is None:
            manifest = "".join(f"run {line}\n" for line in runs)
        build_disk(workdir / "esp.img", manifest, files)
        shutil.copy(OVMF_VARS, workdir / "vars.fd")
        machine = list(ACCELS[accel])
        if cpu:
            machine[-1] = cpu
        # One socket pair per console port: QEMU gets one end as a chardev.
        pairs = [socket.socketpair() for _ in range(CONSOLE_PORTS)]
        cmd = [
            "qemu-system-x86_64", *machine,
            "-m", "256", "-nodefaults", "-display", "none", "-no-reboot",
            "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}",
            "-drive", f"if=pflash,format=raw,unit=1,file={workdir / 'vars.fd'}",
            "-drive", f"if=none,id=esp,format=raw,file={workdir / 'esp.img'}",
            "-device", "virtio-blk-pci,drive=esp",
            "-chardev", f"file,id=log,path={workdir / 'dosh.log'}", "-serial", "chardev:log",
            "-monitor", f"unix:{workdir / 'monitor.sock'},server,nowait",
        ]  # fmt: skip
        for i, (_, theirs) in enumerate(pairs):
            backend = "stdio,signal=off" if attach and i == 0 else f"socket,fd={theirs.fileno()}"
            cmd += ["-chardev", f"{backend},id=con{i}", "-serial", f"chardev:con{i}"]

        started = time.monotonic()
        qemu = subprocess.Popen(
            cmd,
            pass_fds=[theirs.fileno() for _, theirs in pairs],
            stdin=None if attach else subprocess.DEVNULL,
            stdout=None if attach else subprocess.PIPE,
            stderr=subprocess.STDOUT if not attach else None,
        )
        ours = []
        for mine, theirs in pairs:
            theirs.close()
            ours.append(mine)

        # Collect console output until QEMU closes its ends by exiting. A
        # capsule's session starts at its endpoint's banner: input is held
        # back until then, and nothing before it (firmware output) counts.
        consoles = [bytearray() for _ in ours]
        open_socks = dict(zip(ours, consoles))
        unsent = {sock: data for sock, data in zip(ours, stdin or []) if data}
        timed_out = False
        while open_socks:
            remaining = timeout - (time.monotonic() - started)
            ready, _, _ = select.select(list(open_socks), [], [], min(max(remaining, 0), 0.05))
            if nmi and "event=start" in read_text(workdir / "dosh.log"):
                inject_nmi(workdir / "monitor.sock")
                nmi = False
            if not ready:
                if remaining <= 0:
                    timed_out = True
                    qemu.kill()
                    break
                continue
            for sock in ready:
                data = sock.recv(65536)
                if data:
                    open_socks[sock] += data
                    if sock in unsent and session_banner(ours.index(sock)) in open_socks[sock]:
                        sock.sendall(unsent.pop(sock))
                else:
                    del open_socks[sock]
        qemu_output = (qemu.communicate()[0] or b"").decode(errors="replace")
        for sock in ours:
            sock.close()

        log = read_text(workdir / "dosh.log")
        if qemu_output.strip():
            log += f"\n--- qemu ---\n{qemu_output}"
        if keep:
            shutil.copytree(workdir, keep, dirs_exist_ok=True)

    events = parse_events(log)
    sessions = [bytes(c).partition(session_banner(i))[2] for i, c in enumerate(consoles)]
    result = Result(log, events, sessions, time.monotonic() - started, timed_out)
    for event in events:
        if event["subject"] == "capsule" and event.get("event") in ("exit", "fault"):
            result.capsules[int(event["id"])] = event
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("runs", nargs="+", metavar="CAPSULE", help='a program and its arguments, e.g. "ARGS.COM a b"')
    parser.add_argument("--accel", choices=ACCELS, default="tcg" if kvm_unavailable() else "kvm")
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--attach", action="store_true", help="use this terminal as the first capsule's console")
    parser.add_argument("--keep", type=Path, help="keep the run directory (disk image, logs) here")
    parser.add_argument("--corpus", type=Path, metavar="DIR", help="put every file in DIR on drive C:")
    parser.add_argument("--screen", action="store_true", help="show consoles as the 80x25 screen they draw")
    parser.add_argument("--raw", action="store_true", help="print the whole COM1 log, firmware output included")
    args = parser.parse_args()

    stdin = [] if args.attach or sys.stdin.isatty() else [sys.stdin.buffer.read()]
    files = {p.name.upper(): p.read_bytes() for p in args.corpus.iterdir() if p.is_file()} if args.corpus else None
    result = run(
        args.runs, files=files, accel=args.accel, stdin=stdin, timeout=args.timeout, keep=args.keep, attach=args.attach
    )
    for i, console in enumerate(result.consoles[: len(args.runs)]):
        if args.attach and i == 0:
            print()
            continue
        print(f"--- capsule {i} {'screen' if args.screen else 'console'} ---")
        sys.stdout.flush()
        sys.stdout.buffer.write("\n".join(result.screen(i)).encode() if args.screen else console)
        sys.stdout.flush()
        print()
    print(f"--- supervisor log ({args.accel}, {result.seconds:.1f}s) ---")
    if args.raw:
        print(result.log)
    for event in result.events:
        print(" ".join([event["subject"], *(f"{k}={shlex.quote(v)}" for k, v in event.items() if k != "subject")]))
    if result.timed_out:
        print("TIMED OUT")
        return 2
    final = result.capsules.get(0, {})
    return int(final["code"]) if final.get("event") == "exit" else 1


if __name__ == "__main__":
    sys.exit(main())
