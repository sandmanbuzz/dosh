# The guest corpus: small DOS programs, each exercising one part of the
# compatibility contract. Every source assembles to a flat binary; MZ
# executables write their own header (see mz.inc).
OUT := ../build/guests
COM := HELLO A20 EXIT42 RET ARGS UPPER ASK TYPE FILEIO SPIN BADCALL DIVZERO ESCAPE TRIPLE TICK \
       IRQ HLTCLI RATE TIMERHOOK
EXE := HELLOMZ

all: $(COM:%=$(OUT)/%.COM) $(EXE:%=$(OUT)/%.EXE) $(OUT)/WC.COM

# C, through an independent compiler and DOS C library.
$(OUT)/WC.COM: wc.c | $(OUT)
	bcc -Md -ansi -O -o $@ $<

.SECONDEXPANSION:
$(OUT)/%.COM: $$(shell echo $$* | tr A-Z a-z).asm | $(OUT)
	nasm -f bin -Werror -o $@ $<

$(OUT)/%.EXE: $$(shell echo $$* | tr A-Z a-z).asm mz.inc | $(OUT)
	nasm -f bin -Werror -o $@ $<

$(OUT):
	mkdir -p $@
