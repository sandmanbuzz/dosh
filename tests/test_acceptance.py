"""What DOSH can do today, demonstrated end to end.

Every test boots the EFI artifact on virtual bare metal, lets it run
unmodified DOS executables in hardware-virtualized capsules, and checks what
came out of the capsule consoles and what the supervisor reported.
"""

import doshlab


def exited(result, capsule=0):
    """The exit code of a capsule that ran to completion."""
    final = result.capsules.get(capsule)
    assert final and final["event"] == "exit", f"capsule {capsule} did not exit:\n{result.log}"
    return int(final["code"])


def faulted(result, capsule=0):
    """The reason the supervisor stopped a capsule."""
    final = result.capsules.get(capsule)
    assert final and final["event"] == "fault", f"capsule {capsule} did not fault:\n{result.log}"
    return final["reason"]


# --- programs run, produce output, and return a status -------------------


def test_com_program_prints_and_exits(dosh):
    result = dosh("HELLO.COM")
    assert result.consoles[0] == b"Hello from DOS!\r\n"
    assert exited(result) == 0
    assert result.one("shutdown") == {"subject": "shutdown", "capsules": "1", "exited": "1", "faulted": "0"}


def test_exit_code_is_recovered(dosh):
    result = dosh("EXIT42.COM")
    assert exited(result) == 42
    assert result.consoles[0] == b""


def test_com_program_can_end_by_returning(dosh):
    # Also covers BIOS teletype (INT 10h) and DOS fast console (INT 29h).
    result = dosh("RET.COM")
    assert result.consoles[0] == b"returned!\r\n"
    assert exited(result) == 0


def test_a_program_sees_the_bios_tick_advance(dosh):
    # INT 1Ah AH=00h: the guest polls the 32-bit tick count until it has moved.
    result = dosh("TICK.COM")
    assert result.consoles[0] == b"TICK OK\r\n"
    assert exited(result) == 0


def test_a_program_halts_until_the_clock_interrupts_it(dosh):
    # INT 08h handler installed with AH=25h; HLT with IF=1 wakes on each tick.
    result = dosh("IRQ.COM")
    assert result.consoles[0] == b"IRQ OK\r\n"
    assert exited(result) == 0


def test_bios_tick_calls_the_user_timer_hook(dosh):
    # The program hooks INT 1Ch only; the BIOS default INT 08h handler chains to it.
    result = dosh("TIMERHOOK.COM")
    assert result.consoles[0] == b"HOOK OK\r\n"
    assert exited(result) == 0


def test_a_host_nmi_is_survived_and_the_guest_runs_on(dosh):
    # An NMI injected while a capsule runs reaches the supervisor, which counts
    # it and resumes; it must neither power the machine off nor reach the guest.
    result = dosh("RATE.COM", nmi=True)
    assert result.consoles[0] == b"RATE OK\r\n"
    assert exited(result) == 0
    assert int(result.one("nmi")["taken"]) >= 1, result.log
    assert not result.find("trap"), result.log


def test_guest_clock_interrupts_at_the_bios_rate(dosh):
    # INT 08h delivery follows elapsed BIOS time: 16 to 19 ticks in 18 ticks.
    result = dosh("RATE.COM")
    assert result.consoles[0] == b"RATE OK\r\n"
    assert exited(result) == 0


def test_guest_clock_rate_holds_beside_a_busy_capsule(dosh):
    # SPIN keeps the processor busy with interrupts off; the clock must still
    # be delivered to RATE at the BIOS rate while SPIN is running.
    result = dosh("RATE.COM", "SPIN.COM")
    assert result.consoles[0] == b"RATE OK\r\n"
    assert exited(result, 0) == 0


def test_halt_with_interrupts_disabled_faults_with_a_reason(dosh):
    """HLT with IF=0 could never be woken, so the capsule must say so."""
    result = dosh("HLTCLI.COM")
    assert faulted(result) == "HLT with interrupts disabled"

def test_command_line_reaches_the_program(dosh):
    result = dosh("ARGS.COM one  two")
    assert result.consoles[0] == b"[ one  two]\r\n"
    assert exited(result) == len(" one  two")

    result = dosh("ARGS.COM")
    assert result.consoles[0] == b"[]\r\n"
    assert exited(result) == 0


def test_a_program_starts_children_and_resumes(dosh):
    result = dosh("EXEC.COM", files={n: (doshlab.GUESTS / n).read_bytes() for n in ("ARGS.COM", "EXIT42.COM")})
    assert result.consoles[0] == b"[ hi]\r\ncode=03\r\ncode=42\r\nparent resumed\r\n"
    assert exited(result) == 0


def test_the_command_interpreter_runs_commands_and_programs(dosh):
    guests = {n: (doshlab.GUESTS / n).read_bytes() for n in ("ARGS.COM", "EXIT42.COM")}
    session = b"ver\recho  hi there\rtype note.txt\rargs a  b\rexit42\rnope\rdir\rexit\r"
    result = dosh("COMMAND.COM", files={**guests, "NOTE.TXT": b"line one\r\n"}, stdin=[session])
    out = result.consoles[0].decode()
    for want in (
        "DOSH DOS version 5.0\r\n",
        "C:\\>echo  hi there\r\n hi there\r\n",  # ECHO keeps everything after one space
        "line one\r\n",
        "[ a  b]\r\n",
        "Bad command or file name\r\n",
        "ARGS.COM      ",  # DIR lists the files with their sizes
        "NOTE.TXT      10\r\n",
    ):
        assert want in out, f"missing {want!r} in:\n{out}"
    assert exited(result) == 0


def test_the_watchdog_stops_a_capsule_that_never_ends(dosh):
    # The capsule beside it is unaffected: it finishes while the other is cut off.
    result = dosh("limit=1 LOOP.COM", "HELLO.COM")
    assert faulted(result, 0) == "watchdog: used its 1 s of processor time"
    assert exited(result, 1) == 0 and result.consoles[1] == b"Hello from DOS!\r\n"


def test_one_line_carries_many_capsules_one_at_a_time(dosh):
    # Capsule 1 says hello and ends while the line shows others. Its text is
    # held, and arrives after its banner when the line is moved to it.
    script = [
        (b"", b"left\r"),
        (b"LEFT", b"\x1d2"),  # Ctrl-] 2
        (b"capsule 2 ---", b"right\r"),
        (b"RIGHT", b"\x1d1"),
        (b"Hello from DOS!", b"\x1d0\x1a\r\x1d2\x1a\r"),
    ]
    result = dosh("UPPER.COM", "HELLO.COM", "UPPER.COM", stdin=[script], mux=True)
    line = result.consoles[0]
    zero, two, one = (line.partition(b"--- DOSH capsule 2 ---\r\n")[0],) + line.partition(b"--- DOSH capsule 2 ---\r\n")[2].partition(
        b"--- DOSH capsule 1 ---\r\n"
    )[::2]
    assert b"LEFT" in zero and b"RIGHT" not in zero
    assert b"RIGHT" in two and b"Hello" not in two
    assert one.startswith(b"Hello from DOS!\r\n"), line
    assert [exited(result, i) for i in range(3)] == [0, 0, 0]


def test_dos_date_and_time_come_from_the_wall_clock(dosh):
    # The lab sets the virtual RTC to Monday 2031-02-03 12:00:00.
    result = dosh("DATE.COM")
    assert result.consoles[0].startswith(b"2031-02-03 1 12:"), result.consoles[0]
    assert exited(result) == 0


def test_mz_executable_is_relocated_and_runs(dosh):
    result = dosh("HELLOMZ.EXE")
    assert result.consoles[0] == b"Hello from an MZ executable\r\nand from its second code segment\r\n"
    assert exited(result) == 7


def test_guest_code_runs_without_supervision(dosh):
    """A billion and a half guest instructions cost a handful of exits:
    the processor executes the guest, the supervisor only serves its calls."""
    n = 500_000_000
    result = dosh("SPIN.COM")
    assert result.consoles[0] == b"%08X" % ((n * (n + 1) // 2) & 0xFFFFFFFF)
    assert exited(result) == 0
    # Apart from the supervisor's own clock interrupting it, that is.
    final = result.capsules[0]
    assert int(final["vmexits"]) - int(final["preempted"]) < 20


def test_memory_wraps_at_one_megabyte(dosh):
    assert exited(dosh("A20.COM")) == 0


def test_c_program_with_an_independent_runtime(dosh):
    """Compiler, startup code and C library all come from elsewhere (bcc);
    the program is plain C using stdio."""
    text = b"the quick brown fox\r\njumps over\r\n\r\nthe lazy dog\r\n"
    result = dosh("WC.COM fox.txt nope.txt", files={"FOX.TXT": text})
    assert result.consoles[0] == b"      4       9      49 fox.txt\r\nwc: cannot open nope.txt\r\n"
    assert exited(result) == 1


# --- console input ------------------------------------------------------


def test_filter_reads_lines_until_end_of_file(dosh):
    result = dosh("UPPER.COM", stdin=[b"hello\rdos world\r\x1a\r"])
    # Cooked input is echoed by DOS; each line comes back in upper case.
    assert result.consoles[0] == b"hello\r\nHELLO\r\ndos world\r\nDOS WORLD\r\n\x1a\r\n"
    assert exited(result) == 0


def test_interactive_line_and_keystroke(dosh):
    result = dosh("ASK.COM", stdin=[b"Adx\x08a\rq"])
    assert result.consoles[0] == b"Name? Adx\x08 \x08a\r\nHello, Ada!\r\nKey? "
    assert exited(result) == ord("q")


# --- files on drive C: --------------------------------------------------


def test_program_reads_a_file_named_on_its_command_line(dosh):
    notes = b"".join(b"line %d of the notes\r\n" % i for i in range(100))
    result = dosh("TYPE.COM notes.txt", files={"NOTES.TXT": notes})
    assert result.consoles[0] == notes
    assert exited(result) == 0

    result = dosh("TYPE.COM missing.txt")
    assert result.consoles[0] == b"File not found\r\n"
    assert exited(result) == 2


def test_program_creates_writes_seeks_and_rereads_a_file(dosh):
    result = dosh("FILEIO.COM")
    assert result.consoles[0] == b"Hello, WORLD of files\r\n"
    assert exited(result) == 0


# --- misbehaving guests are contained and explained -----------------------


def test_unimplemented_service_stops_the_capsule_and_names_the_call(dosh):
    result = dosh("BADCALL.COM")
    assert faulted(result) == "unimplemented service INT 21h AH=FFh AL=42h"
    assert result.one("shutdown")["faulted"] == "1"


def test_unhandled_processor_exception_stops_the_capsule(dosh):
    assert faulted(dosh("DIVZERO.COM")) == "unhandled processor exception 0"


def test_leaving_real_mode_is_refused(dosh):
    assert "forbidden operation" in faulted(dosh("ESCAPE.COM"))


def test_triple_fault_takes_down_only_the_capsule(dosh):
    result = dosh("TRIPLE.COM")
    assert faulted(result) == "triple fault"
    result.one("shutdown")  # the supervisor carried on to an orderly end


def test_missing_program_is_reported(dosh):
    assert "cannot load NOSUCH.COM" in faulted(dosh("NOSUCH.COM"))


# --- more than one capsule ------------------------------------------------


def test_capsules_are_independent(dosh):
    result = dosh("ARGS.COM first", "ESCAPE.COM", "ARGS.COM third")
    assert result.consoles[0] == b"[ first]\r\n"
    assert result.consoles[2] == b"[ third]\r\n"
    assert exited(result, 0) == len(" first")
    assert "forbidden operation" in faulted(result, 1)
    assert exited(result, 2) == len(" third")
    assert result.one("shutdown") == {"subject": "shutdown", "capsules": "3", "exited": "2", "faulted": "1"}


def test_a_busy_capsule_cannot_keep_the_processor(dosh):
    # SPIN computes for a long time with interrupts off and makes no calls.
    # The capsule queued behind it must still run, and finish first.
    result = dosh("SPIN.COM", "HELLO.COM")
    assert result.consoles[1] == b"Hello from DOS!\r\n"
    finished = [int(e["id"]) for e in result.find("capsule", event="exit")]
    assert finished == [1, 0], result.log
    assert int(result.capsules[0]["preempted"]) > 0


def test_capsules_wait_for_input_side_by_side(dosh):
    # Both are blocked reading their own consoles at the same time; each is
    # answered only from its own endpoint.
    result = dosh("UPPER.COM", "UPPER.COM", stdin=[b"left\r\x1a\r", b"right\r\x1a\r"])
    assert result.consoles[0] == b"left\r\nLEFT\r\n\x1a\r\n"
    assert result.consoles[1] == b"right\r\nRIGHT\r\n\x1a\r\n"
    assert exited(result, 0) == 0 and exited(result, 1) == 0


def test_a_terminal_can_be_attached_to_a_capsule():
    """`doshlab.py --attach` puts a person at the capsule's console. Drive
    it through a pseudo-terminal the way a person would: wait for a prompt,
    then type."""
    import os
    import pty
    import subprocess
    import sys

    master, slave = pty.openpty()
    cmd = [sys.executable, doshlab.__file__, "--attach", "--accel", "tcg", "ASK.COM"]
    proc = subprocess.Popen(cmd, stdin=slave, stdout=slave, stderr=subprocess.STDOUT)
    os.close(slave)
    seen = b""

    def wait_for(prompt):
        nonlocal seen
        while prompt not in seen:
            seen += os.read(master, 4096)

    wait_for(b"Name? ")
    os.write(master, b"Pat\r")
    wait_for(b"Key? ")
    os.write(master, b"k")
    try:
        while True:
            seen += os.read(master, 4096)
    except OSError:  # the far end closed: doshlab has finished
        pass
    assert b"Hello, Pat!" in seen
    assert proc.wait(timeout=30) == ord("k"), seen.decode(errors="replace")


# --- the supervisor on its own ---------------------------------------------


def test_booting_is_not_the_same_as_running_a_guest():
    """On a processor without SVM, DOSH boots, says so, and runs nothing.
    A successful boot is never evidence of guest execution."""
    result = doshlab.run(["HELLO.COM"], accel="tcg", cpu="max,svm=off")
    assert not result.timed_out, result.log
    result.one("boot", stage="supervisor")
    assert result.one("hv")["status"] == "unavailable"
    assert result.capsules == {}
    assert result.consoles[0] == b""
    assert result.one("shutdown")["capsules"] == "0"


def test_bad_manifest_is_reported():
    result = doshlab.run([], accel="tcg", manifest="# typo below\nlaunch HELLO.COM\n")
    assert not result.timed_out, result.log
    assert "unknown directive: launch HELLO.COM" in result.find("boot")[-1]["error"]
    assert result.one("shutdown")["capsules"] == "0"


# --- third-party programs from the local corpus --------------------------------
# The corpus is not redistributed with the repository: these tests skip when a
# binary is absent.

import struct
from pathlib import Path

import pytest

CORPUS = Path(__file__).resolve().parent.parent / "corpus"


def corpus_file(*parts):
    path = CORPUS.joinpath(*parts)
    if not path.exists():
        pytest.skip(f"corpus/{'/'.join(parts)} not present")
    return path.read_bytes()


def arc_crc(data):
    crc = 0
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ 0xA001 if crc & 1 else crc >> 1
    return crc


def stored_arc(name, data, date=0x2A4B, time=0x6000):
    """A one-member ARC archive using the "stored" method."""
    header = b"\x1a\x02" + name.encode().ljust(13, b"\0")
    header += struct.pack("<IHHHI", len(data), date, time, arc_crc(data), len(data))
    return header + data + b"\x1a\x00"


def test_arc_lists_and_prints_an_archive(dosh):
    body = b"Stored inside an archive.\n" * 4
    files = {"ARC.EXE": corpus_file("early", "ARC.EXE"), "T.ARC": stored_arc("HELLO.TXT", body)}
    listing = dosh("ARC.EXE l T.ARC", files=files)
    assert exited(listing) == 0
    # The length, and the date stored in the archive.
    assert b"HELLO.TXT" in listing.consoles[0] and str(len(body)).encode() in listing.consoles[0]
    assert b"11 Feb 01" in listing.consoles[0]
    shown = dosh("ARC.EXE p T.ARC", files=files)
    assert exited(shown) == 0
    # ARC prints members as text, turning each newline into a line break.
    assert body.replace(b"\n", b"\r\n") in shown.consoles[0]


def test_arc_extracts_and_rejects_a_corrupt_member(dosh):
    body = b"Checked by CRC.\r\n" * 8
    files = {"ARC.EXE": corpus_file("early", "ARC.EXE")}
    ok = dosh("ARC.EXE x T.ARC", files={**files, "T.ARC": stored_arc("A.TXT", body)})
    assert exited(ok) == 0 and b"Extracting file: A.TXT" in ok.consoles[0]
    bad = bytearray(stored_arc("A.TXT", body))
    bad[40] ^= 0xFF  # flip a byte of the member
    broken = dosh("ARC.EXE x T.ARC", files={**files, "T.ARC": bytes(bad)})
    assert exited(broken) == 1 and b"File A.TXT fails CRC check" in broken.consoles[0]


def test_arc_crunches_a_file_into_a_new_archive(dosh):
    text = b"the quick brown fox jumps over the lazy dog\r\n" * 40
    result = dosh("ARC.EXE a T.ARC FOX.TXT", files={"ARC.EXE": corpus_file("early", "ARC.EXE"), "FOX.TXT": text})
    assert exited(result) == 0
    assert b"Adding file:   FOX.TXT" in result.consoles[0] and b"done" in result.consoles[0]
    size, _ = result.files()["T.ARC"]
    assert 0 < size < len(text) // 3, "a repetitive file crunches to a fraction of its size"


# --- the text display and the keyboard ------------------------------------------


def test_screen_program_draws_and_reads_a_key(dosh):
    up_arrow = b"\x1b[A"
    result = dosh("SCREEN.COM", stdin=[up_arrow])
    assert exited(result) == 0x48, "INT 16h reports the up arrow as scan code 48h"
    screen = result.screen()
    assert screen[2] == " " * 10 + "Hello, screen", "DOS console output lands at the cursor"
    assert screen[5] == "Direct store", "a direct store into B800:0000"
    assert screen[6] == "╔", "code page 437 glyphs reach the terminal"
    assert screen[8] == "scan=48"


def test_a_screen_is_redrawn_when_its_capsule_is_shown_again(dosh):
    up_arrow = b"\x1b[A"
    script = [
        (b"Direct store", b"\x1d1"),
        (b"Direct store", b"\x1d0"),  # capsule 1 has drawn its screen
        (b"capsule 0 ---", up_arrow),  # back on capsule 0: its screen is drawn afresh, then it ends
        (b"scan=48", b"\x1d1" + up_arrow),
    ]
    result = dosh("SCREEN.COM", "SCREEN.COM", stdin=[script], mux=True, timeout=60)
    assert [exited(result, i) for i in range(2)] == [0x48, 0x48]
    first, _, rest = result.consoles[0].partition(b"\r\n--- DOSH capsule 1 ---\r\n")
    second, _, again = rest.partition(b"\r\n--- DOSH capsule 0 ---\r\n")
    drawn = doshlab.render_screen(first)
    assert drawn[2] == " " * 10 + "Hello, screen" and drawn[5] == "Direct store"
    assert doshlab.render_screen(second)[:7] == drawn[:7], "capsule 1 shows the same program"
    # Capsule 0 comes back with nothing carried over from capsule 1's drawing.
    last = doshlab.render_screen(again.partition(b"--- DOSH capsule 1 ---")[0])
    assert last[:7] == drawn[:7] and last[8] == "scan=48", "\n".join(last)


def test_screen_programs_start_as_a_byte_stream(dosh):
    # Nothing here touches the video BIOS, so the console stays a stream.
    result = dosh("HELLO.COM")
    assert result.consoles[0].startswith(b"Hello from DOS!")
    assert b"\x1b[" not in result.consoles[0]


@pytest.mark.parametrize(
    "folder, program, banner",
    [
        ("earliest", "TURBO.COM", "TURBO Pascal system"),
        ("earliest", "VC.COM", "VisiCalc Advanced Version"),
        ("early", "WS.COM", "O P E N I N G   M E N U"),
    ],
)
def test_full_screen_programs_put_up_their_first_screen(accel, folder, program, banner):
    """Each of these waits for a person; the first screen is the proof."""
    files = {program: corpus_file(folder, program)}
    for extra in (CORPUS / folder).iterdir():
        files.setdefault(extra.name, extra.read_bytes())
    result = doshlab.run([program], accel=accel, files=files, timeout=10)
    assert result.timed_out, f"the program should be waiting for a key:\n{result.log}"
    assert not result.find("capsule", event="fault"), result.log
    assert any(banner in row for row in result.screen()), "\n".join(result.screen())


def test_wordstar_saves_a_document(dosh):
    """Type into WordStar, save and exit: the file exists on C: with our text."""
    files = {p.name: p.read_bytes() for p in (CORPUS / "early").iterdir()} if (CORPUS / "early").exists() else {}
    corpus_file("early", "WS.COM")
    keys = b" \rdNOTE.TXT\rHello from WordStar\r\x0bx"  # key, open NOTE.TXT, type, ^KX
    result = dosh("WS.COM", files=files, stdin=[keys], timeout=60)
    assert exited(result) == 0
    text = b"Hello from WordStar\r\n"
    saved = text + b"\x1a" * (128 - len(text))  # padded to a record with ^Z
    assert result.files() == {"NOTE.TXT": (len(saved), doshlab.fnv1a(saved))}


def test_turbo_pascal_compiles_and_runs_a_program_started_from_the_shell(dosh):
    """COMMAND.COM starts Turbo Pascal; in it: new work file, type a program,
    compile and run it, quit back to the shell, exit."""
    files = {p.name: p.read_bytes() for p in (CORPUS / "early").iterdir()} if (CORPUS / "early").exists() else {}
    corpus_file("early", "TURBO.COM")
    files["COMMAND.COM"] = (doshlab.GUESTS / "COMMAND.COM").read_bytes()
    # Turbo aborts a compilation on any keypress, so quit only once it has run.
    script = [(b"", b"turbo\rnewt.pas\rbegin writeln('hi from pascal') end.\x0bdr"), (b"hi from pascal", b"qyexit\r")]
    result = dosh("COMMAND.COM", files=files, stdin=[script], timeout=60)
    screen = "\n".join(result.screen())
    assert "Compiling" in screen and "hi from pascal" in screen, screen
    assert exited(result) == 0
    assert "WT.PAS" in result.files(), result.files()  # saved on the way out, through handle writes


def test_a_program_built_by_a_compiler_in_the_capsule_runs_in_the_capsule(dosh):
    """Turbo Pascal writes a .COM file; the shell then runs it. The compiler
    is a guest of DOSH, and so is what it makes."""
    files = {p.name: p.read_bytes() for p in (CORPUS / "early").iterdir()} if (CORPUS / "early").exists() else {}
    corpus_file("early", "TURBO.COM")
    files["COMMAND.COM"] = (doshlab.GUESTS / "COMMAND.COM").read_bytes()
    # Options: compile to a Com-file; Compile; quit and save the source; run the result.
    script = [
        (b"", b"turbo\rnewt.pas\rbegin writeln('built here, run here') end.\x0bdocqc"),
        (b"Code:", b"qywt\r"),
        (b"built here, run here", b"exit\r"),
    ]
    result = dosh("COMMAND.COM", files=files, stdin=[script], timeout=60)
    assert "built here, run here" in "\n".join(result.screen())
    assert exited(result) == 0
    assert {"WT.COM", "WT.PAS"} <= set(result.files()), result.files()


# --- a supervisor that stays up -------------------------------------------


def order(result, *needles):
    """Positions of each needle in the supervisor log; asserts they all exist."""
    spots = [result.log.find(n) for n in needles]
    assert all(s >= 0 for s in spots), f"missing one of {needles}\n{result.log}"
    return spots


def test_operator_can_suspend_resume_and_kill_capsules(dosh):
    """Launch a long computation, suspend it, run another capsule to the end
    while it sleeps, resume it to the right answer, and kill a capsule that
    never ends."""
    n, rounds = 400_000_000, 6
    expected = b"%08X" % ((rounds * (n * (n + 1) // 2)) & 0xFFFFFFFF)
    result = dosh(
        mgmt=[
            ("mgmt event=ready", "launch LONG.COM"),
            ("cmd=launch status=ok id=0", "suspend 0"),
            ("cmd=suspend status=ok", "launch HELLO.COM"),
            ("cmd=launch status=ok id=1", "launch LOOP.COM"),
            ("capsule id=1 event=exit", "list"),
            ("cmd=list status=ok", "kill 2"),
            ("cmd=kill status=ok", "resume 0"),
            ("capsule id=0 event=exit", "list"),
            ("cmd=list status=ok count=3", "shutdown"),
        ],
        stock=["LONG.COM", "HELLO.COM", "LOOP.COM"],
        timeout=120,
    )
    assert exited(result, 0) == 0
    assert result.consoles[0] == expected
    assert result.consoles[1] == b"Hello from DOS!\r\n" and exited(result, 1) == 0
    # The sequence the operator drove, in order: LONG slept while HELLO ran.
    suspend, hello_done, resume, long_done = order(
        result, "capsule id=0 event=suspend", "capsule id=1 event=exit", "capsule id=0 event=resume", "capsule id=0 event=exit"
    )
    assert suspend < hello_done < resume < long_done
    assert result.one("capsule", id="2", event="kill")
    assert "mgmt item=capsule id=2 program=LOOP.COM status=killed" in result.log
    assert "mgmt item=capsule id=0 program=LONG.COM status=suspended" not in result.log.split("event=resume")[1]


def test_operator_errors_are_answered_and_change_nothing(dosh):
    result = dosh(
        mgmt=[
            ("mgmt event=ready", "frobnicate"),
            ("unknown command: frobnicate", "suspend 7"),
            ("no capsule 7", "launch NOPE.COM"),
            ("no such program: NOPE.COM", "launch"),
            ("launch needs a program", "launch HELLO.COM"),
            ("cmd=launch status=ok id=0", "resume 0"),
            ("capsule id=0 event=exit", "kill 0"),
            ("cmd=kill status=error", "shutdown"),
        ],
        stock=["HELLO.COM"],
    )
    assert "capsule 0 is not suspended" in result.log
    assert "capsule 0 is not running" in result.log
    assert exited(result, 0) == 0


# --- a drive that persists ------------------------------------------------


def test_a_programs_output_file_survives_a_reboot(dosh, tmp_path):
    disk = doshlab.make_disk(tmp_path / "data.img")
    for boot in (1, 2, 3):
        result = dosh("COUNTER.COM", disk=disk)
        assert result.consoles[0] == b"%03d" % boot
        assert exited(result) == boot
        result.one("storage", event="attach", label="DOSHDATA")
        result.one("storage", event="write", capsule="0", name="COUNT.DAT", size="1")
    # What the host's own FAT tools read back is what the supervisor wrote.
    assert doshlab.read_disk_files(disk) == {"COUNT.DAT": b"\x03"}


def test_a_disk_prepared_on_the_host_is_read_and_extended(dosh, tmp_path):
    disk = doshlab.make_disk(tmp_path / "data.img", {"COUNT.DAT": b"\x29", "NOTES.TXT": b"left alone"})
    result = dosh("COUNTER.COM", disk=disk)
    assert result.consoles[0] == b"042"
    assert doshlab.read_disk_files(disk) == {"COUNT.DAT": b"\x2a", "NOTES.TXT": b"left alone"}


def test_a_program_that_faults_leaves_the_disk_alone(dosh, tmp_path):
    disk = doshlab.make_disk(tmp_path / "data.img", {"COUNT.DAT": b"\x07"})
    result = dosh("limit=1 LOOP.COM", disk=disk)
    assert "watchdog" in faulted(result)
    assert doshlab.read_disk_files(disk) == {"COUNT.DAT": b"\x07"}
    assert not result.find("storage", event="write")


def test_work_can_be_loaded_after_boot_and_its_output_kept(dosh, tmp_path):
    """The operator stores a program on the disk over the management channel,
    launches it though no boot line named it, and reads a file it made back."""
    disk = doshlab.make_disk(tmp_path / "data.img")
    counter = (doshlab.GUESTS / "COUNTER.COM").read_bytes()
    result = dosh(
        disk=disk,
        mgmt=[
            ("mgmt event=ready", "files"),
            ("cmd=files status=ok count=0", f"put COUNTER.COM {counter.hex()}"),
            ("cmd=put status=ok", "launch COUNTER.COM"),
            ("capsule id=0 event=exit code=1", "launch COUNTER.COM"),
            ("capsule id=1 event=exit code=2", "files"),
            ("cmd=files status=ok count=2", "get COUNT.DAT"),
            ("cmd=get status=ok", "shutdown"),
        ],
    )
    assert result.consoles[0] == b"001" and result.consoles[1] == b"002"
    assert "mgmt item=file name=COUNTER.COM size=%d" % len(counter) in result.log
    assert "name=COUNT.DAT size=1 hex=02" in result.log
    assert doshlab.read_disk_files(disk) == {"COUNTER.COM": counter, "COUNT.DAT": b"\x02"}


def test_without_a_disk_nothing_persists_and_the_operator_is_told(dosh):
    result = dosh(
        "COUNTER.COM",
        mgmt=[("capsule id=0 event=exit", "files"), ("cmd=files status=error", "put A.TXT 41"), ("cmd=put status=error", "shutdown")],
    )
    assert result.consoles[0] == b"001"
    assert not result.find("storage", event="attach")
    assert "no persistent disk" in result.log


def test_a_disk_that_is_not_ours_is_left_alone(dosh, tmp_path):
    disk = doshlab.make_disk(tmp_path / "data.img", {"COUNT.DAT": b"\x05"})
    subprocess = __import__("subprocess")
    subprocess.run(["mlabel", "-i", str(disk), "::SOMEONELSE"], check=True, env={"MTOOLS_SKIP_CHECK": "1", "PATH": "/usr/bin:/bin"})
    result = dosh("COUNTER.COM", disk=disk)
    assert result.consoles[0] == b"001"
    assert not result.find("storage", event="attach")
    assert doshlab.read_disk_files(disk) == {"COUNT.DAT": b"\x05"}


# --- a persistent environment, reachable over TCP -------------------------


def test_a_served_supervisor_is_operated_and_attached_over_tcp(accel, tmp_path):
    """The whole point: start DOSH once, reach it from outside the way
    `socat` would, run work, shut it down, start it again, and find the
    work's output still there."""
    import socket as sockets
    import time

    disk = doshlab.make_disk(tmp_path / "data.img", megabytes=32)

    def free_port():
        with sockets.socket() as s:
            s.bind(("127.0.0.1", 0))
            return s.getsockname()[1]

    def connect(port, deadline):
        while time.monotonic() < deadline:
            try:
                return sockets.create_connection(("127.0.0.1", port), timeout=1)
            except OSError:
                time.sleep(0.1)
        raise AssertionError(f"nothing listening on {port}")

    seen = {}

    def read_until(sock, marker, deadline):
        """Read until `marker` has arrived on this line; what came before
        and including it is consumed, the rest stays for the next call."""
        buffer = seen.setdefault(sock, bytearray())
        sock.settimeout(1)
        while marker not in buffer:
            assert time.monotonic() < deadline, f"never saw {marker!r} in {bytes(buffer[-400:])!r}"
            try:
                chunk = sock.recv(65536)
            except TimeoutError:
                continue
            assert chunk, f"line closed before {marker!r}: {bytes(buffer[-400:])!r}"
            buffer += chunk
        end = buffer.index(marker) + len(marker)
        consumed = bytes(buffer[:end])
        del buffer[:end]
        return consumed

    def session(boot):
        mgmt_port, console_port = free_port(), free_port()
        work = tmp_path / f"serve{boot}"
        work.mkdir()
        qemu = doshlab.serve(
            disk, accel=accel, mgmt_port=mgmt_port, console_port=console_port,
            programs=[doshlab.GUESTS / "COUNTER.COM", doshlab.GUESTS / "HELLO.COM"], workdir=work,
        )  # fmt: skip
        deadline = time.monotonic() + (120 if accel == "tcg" else 60)
        try:
            mgmt, console = connect(mgmt_port, deadline), connect(console_port, deadline)
            read_until(mgmt, b"mgmt event=ready", deadline)
            mgmt.sendall(b"launch COUNTER.COM\n")
            read_until(mgmt, b"cmd=launch status=ok id=0", deadline)
            # Firmware chatters on every serial line before DOSH owns it; the
            # capsule's session begins at its banner.
            read_until(console, b"--- DOSH capsule 0 ---\r\n", deadline)
            read_until(console, b"00%d" % boot, deadline)
            read_until(mgmt, b"capsule id=0 event=exit code=%d" % boot, deadline)
            mgmt.sendall(b"shutdown\n")
            read_until(mgmt, b"shutdown capsules=1", deadline)
            assert qemu.wait(timeout=30) == 0
        finally:
            qemu.kill()
            qemu.communicate()

    session(1)
    assert doshlab.read_disk_files(disk) == {"COUNT.DAT": b"\x01"}
    session(2)
    assert doshlab.read_disk_files(disk) == {"COUNT.DAT": b"\x02"}


def test_a_supervisor_that_stays_up_gives_finished_capsules_back(dosh):
    """Eighty one-megabyte capsules do not fit in the supervisor's memory at
    once, so this only works if a finished capsule releases its machine."""
    count = 80
    steps = [("mgmt event=ready", "launch HELLO.COM")]
    for i in range(count - 1):
        steps.append((f"capsule id={i} event=exit", "launch HELLO.COM"))
    steps += [(f"capsule id={count - 1} event=exit", "list"), ("cmd=list status=ok", "shutdown")]
    result = dosh(mgmt=steps, stock=["HELLO.COM"], timeout=170)
    assert result.one("shutdown")["exited"] == str(count)
    assert "status=error" not in result.log
