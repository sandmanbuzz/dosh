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
