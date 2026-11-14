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
# Serial ports after COM1 (the supervisor log and management channel): one
# console per capsule.
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

    def files(self, capsule: int = 0) -> dict[str, tuple[int, str]]:
        """Files the capsule's program created or changed, as name -> (size,
        FNV-1a of the contents). The volume is not written back, so the
        supervisor reports these when the program exits."""
        found = self.find("file", capsule=str(capsule))
        return {e["name"]: (int(e["size"]), e["fnv1a"]) for e in found}

    def find(self, subject: str, **match: str) -> list[dict[str, str]]:
        return [e for e in self.events if e["subject"] == subject and all(e.get(k) == v for k, v in match.items())]

    def one(self, subject: str, **match: str) -> dict[str, str]:
        found = self.find(subject, **match)
        assert len(found) == 1, f"expected one {subject} {match} event, got {found}\n{self.log}"
        return found[0]


def fnv1a(data: bytes) -> str:
    """The content fingerprint the supervisor logs for changed files."""
    h = 0x811C9DC5
    for b in data:
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return f"{h:08x}"


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


def make_disk(image: Path, files: dict[str, bytes] | None = None, megabytes: int = 16, name: str = "DATA") -> Path:
    """A persistent volume for the supervisor: FAT, labelled DOSH plus `name`
    (DOSHDATA, the default volume, unless told otherwise), holding `files`. The same image can be handed to run() any number of times; what
    programs write to it survives from one boot to the next."""
    env = {**os.environ, "MTOOLS_SKIP_CHECK": "1"}
    with open(image, "wb") as f:
        f.truncate(megabytes << 20)
    subprocess.run(["mformat", "-i", str(image), "-T", str(megabytes * 2048), "-v", "DOSH" + name, "::"], check=True, env=env)
    for name, data in (files or {}).items():
        write_disk_file(image, name, data)
    return image


def write_disk_file(image: Path, name: str, data: bytes) -> None:
    """Put a file on a disk image while no supervisor has it open."""
    env = {**os.environ, "MTOOLS_SKIP_CHECK": "1"}
    with tempfile.TemporaryDirectory() as tmp:
        (Path(tmp) / name).write_bytes(data)
        subprocess.run(["mcopy", "-o", "-i", str(image), str(Path(tmp) / name), f"::/{name}"], check=True, env=env)


def read_disk_files(image: Path) -> dict[str, bytes]:
    """What a disk image holds, read with the host's own FAT tools: an
    independent check on what the supervisor wrote."""
    env = {**os.environ, "MTOOLS_SKIP_CHECK": "1"}
    listing = subprocess.run(["mdir", "-b", "-i", str(image), "::"], check=True, env=env, capture_output=True, text=True)
    files = {}
    for line in listing.stdout.split():
        name = line.rsplit("/", 1)[-1]
        data = subprocess.run(["mtype", "-i", str(image), f"::/{name}"], check=True, env=env, capture_output=True)
        files[name.upper()] = data.stdout
    return files


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
    mux: bool = False,
    disk: Path | list[Path] | None = None,
    stay: bool | None = None,
    mgmt: list[tuple[str, str]] | None = None,
    stock: list[str] | None = None,
    timeout: float = 120,
    keep: Path | None = None,
) -> Result:
    """Boot DOSH with one capsule per entry of `runs` and wait for it to
    power off.

    `stdin[i]` is sent to capsule i's console. `files` adds to drive C:,
    which always holds the programs named in `runs`. `cpu` overrides the
    CPU model; `manifest` replaces the generated boot manifest verbatim. `mux` puts every
    capsule on the first console line, switched with Ctrl-] and a number.
    `attach` connects capsule 0's console to this process's own terminal
    instead of capturing it (firmware output included, as on a real line).
    `nmi` injects one non-maskable interrupt once the first capsule has
    started. `disk` attaches persistent disk images, one or a list (see make_disk). `mgmt`
    is a script for the management channel on COM1, as (wait_for, command)
    steps: each command goes out once `wait_for` has appeared in the
    supervisor log since the previous one, and giving one makes the
    supervisor stay up (`stay`) so the script can end it with `shutdown`.
    `stock` names programs from build/guests to put on drive C: even though no
    boot line runs them: the operator will.
    `keep` preserves the run directory there for inspection.
    """
    files = dict(files or {})
    for program in stock or []:
        files.setdefault(program, (GUESTS / program).read_bytes())
    for line in runs:
        program = next(word for word in line.split() if not word.startswith(("limit=", "vol=")))
        if (GUESTS / program).exists():
            files.setdefault(program, (GUESTS / program).read_bytes())

    with tempfile.TemporaryDirectory(prefix="dosh-run-") as tmp:
        workdir = Path(tmp)
        if manifest is None:
            stay = mgmt is not None if stay is None else stay
            manifest = ("console mux\n" if mux else "") + ("supervisor stay\n" if stay else "")
            manifest += "".join(f"run {line}\n" for line in runs)
        build_disk(workdir / "esp.img", manifest, files)
        shutil.copy(OVMF_VARS, workdir / "vars.fd")
        machine = list(ACCELS[accel])
        if cpu:
            machine[-1] = cpu
        # One socket pair per console port: QEMU gets one end as a chardev.
        pairs = [socket.socketpair() for _ in range(CONSOLE_PORTS + 1)]  # COM1 first
        cmd = [
            "qemu-system-x86_64", *machine,
            "-m", "256", "-rtc", "base=2031-02-03T12:00:00,clock=vm", "-nodefaults", "-display", "none", "-no-reboot",
            "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}",
            "-drive", f"if=pflash,format=raw,unit=1,file={workdir / 'vars.fd'}",
            "-drive", f"if=none,id=esp,format=raw,file={workdir / 'esp.img'}",
            "-device", "virtio-blk-pci,drive=esp",
            "-monitor", f"unix:{workdir / 'monitor.sock'},server,nowait",
        ]  # fmt: skip
        for n, image in enumerate([disk] if isinstance(disk, Path) else disk or []):
            cmd += [
                "-drive", f"if=none,id=data{n},format=raw,file={image}",
                "-device", f"virtio-blk-pci,drive=data{n},disable-modern=on,disable-legacy=off",
            ]  # fmt: skip
        for i, (_, theirs) in enumerate(pairs):
            backend = "stdio,signal=off" if attach and i == 1 else f"socket,fd={theirs.fileno()}"
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

        # Collect output until QEMU closes its ends by exiting. COM1, the
        # supervisor log, is the first socket; the capsule consoles follow. A
        # capsule's session starts at its endpoint's banner: input is held
        # back until then, and nothing before it (firmware output) counts.
        logbuf = bytearray()
        consoles = [bytearray() for _ in ours[1:]]
        buffers = {ours[0]: logbuf, **dict(zip(ours[1:], consoles))}
        open_socks = dict(buffers)
        # Input is a script of (wait_for, send) steps; plain bytes are one
        # step that goes out as soon as the session starts. A step waits until
        # its marker has appeared in the output since the previous step.
        unsent = {}
        for sock, data in zip(ours[1:], stdin or []):
            steps = [(b"", data)] if isinstance(data, bytes) else list(data)
            if steps and any(send for _, send in steps):
                unsent[sock] = steps
        if mgmt:
            unsent[ours[0]] = [(w.encode(), c.encode() + b"\n") for w, c in mgmt]
        mark = {sock: 0 for sock in ours}
        timed_out = False
        while open_socks:
            remaining = timeout - (time.monotonic() - started)
            ready, _, _ = select.select(list(open_socks), [], [], min(max(remaining, 0), 0.05))
            if nmi and b"event=start" in logbuf:
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
                if not data:
                    del open_socks[sock]
                    continue
                buffers[sock] += data
                seen = buffers[sock]
                # The log has no session to wait for; a console's starts at its banner.
                banner = b"" if sock is ours[0] else session_banner(ours.index(sock) - 1)
                while sock in unsent and banner in seen:
                    wait_for, send = unsent[sock][0]
                    start = max(mark[sock], seen.index(banner) + len(banner))
                    if wait_for not in seen[start:]:
                        break
                    sock.sendall(send)
                    mark[sock] = len(seen)
                    unsent[sock].pop(0)
                    if not unsent[sock]:
                        del unsent[sock]
        qemu_output = (qemu.communicate()[0] or b"").decode(errors="replace")
        for sock in ours:
            sock.close()

        log = logbuf.decode(errors="replace")
        (workdir / "dosh.log").write_text(log)
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


def serve(
    disk: Path,
    *,
    accel: str = "tcg",
    bind: str = "127.0.0.1",
    mgmt_port: int = 7000,
    console_port: int = 7001,
    programs: list[Path] | None = None,
    runs: list[str] | None = None,
    workdir: Path,
) -> subprocess.Popen:
    """Start a DOSH that stays up, with its two lines on TCP.

    `mgmt_port` carries the supervisor log and the management channel;
    `console_port` carries every capsule's console, switched with Ctrl-] and
    a digit. `disk` is the persistent disk image (made if it does not exist).
    `programs` are put on the base drive C:, defaulting to every guest built
    in build/guests. The caller owns `workdir` and the returned QEMU process.
    """
    if not disk.exists():
        make_disk(disk, megabytes=64)
    files = {p.name.upper(): p.read_bytes() for p in (programs if programs is not None else sorted(GUESTS.iterdir()))}
    manifest = "console mux\nsupervisor stay\n" + "".join(f"run {line}\n" for line in runs or [])
    build_disk(workdir / "esp.img", manifest, files)
    shutil.copy(OVMF_VARS, workdir / "vars.fd")
    listen = lambda port: f"socket,host={bind},port={port},server=on,wait=off"  # noqa: E731
    cmd = [
        "qemu-system-x86_64", *ACCELS[accel],
        "-m", "256", "-nodefaults", "-display", "none", "-no-reboot",
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={OVMF_CODE}",
        "-drive", f"if=pflash,format=raw,unit=1,file={workdir / 'vars.fd'}",
        "-drive", f"if=none,id=esp,format=raw,file={workdir / 'esp.img'}",
        "-device", "virtio-blk-pci,drive=esp",
        "-drive", f"if=none,id=data,format=raw,file={disk}",
        "-device", "virtio-blk-pci,drive=data,disable-modern=on,disable-legacy=off",
        "-chardev", listen(mgmt_port) + ",id=mgmt", "-serial", "chardev:mgmt",
        "-chardev", listen(console_port) + ",id=console", "-serial", "chardev:console",
    ]  # fmt: skip
    return subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)


def serve_main(args: argparse.Namespace) -> int:
    with tempfile.TemporaryDirectory(prefix="dosh-serve-") as tmp:
        qemu = serve(
            args.disk,
            accel=args.accel,
            bind=args.bind,
            mgmt_port=args.mgmt_port,
            console_port=args.console_port,
            programs=list(args.programs) if args.programs else None,
            runs=args.run,
            workdir=Path(tmp),
        )
        print(f"DOSH is up ({args.accel}); persistent disk {args.disk}", flush=True)
        print(f"  operate:  socat - TCP:{args.bind}:{args.mgmt_port}   (list, launch PROG, suspend N, resume N, kill N, files, shutdown)")
        print(f"  console:  socat -,raw,echo=0 TCP:{args.bind}:{args.console_port}   (Ctrl-] then a digit switches capsule)", flush=True)
        try:
            return qemu.wait()
        except KeyboardInterrupt:
            qemu.kill()
            return 130


def main() -> int:
    if sys.argv[1:2] == ["serve"]:
        parser = argparse.ArgumentParser(prog="doshlab.py serve", description=serve.__doc__)
        parser.add_argument("--disk", type=Path, default=ROOT / "build/data.img", help="persistent disk image")
        parser.add_argument("--accel", choices=ACCELS, default="tcg" if kvm_unavailable() else "kvm")
        parser.add_argument("--bind", default="127.0.0.1", help="address to listen on; the lines are unauthenticated")
        parser.add_argument("--mgmt-port", type=int, default=7000)
        parser.add_argument("--console-port", type=int, default=7001)
        parser.add_argument("--run", action="append", metavar="CAPSULE", help="a capsule to start at boot")
        parser.add_argument("programs", nargs="*", type=Path, help="files for drive C: (default: build/guests)")
        return serve_main(parser.parse_args(sys.argv[2:]))
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
