/* WC.COM - count the lines, words and bytes of the files named on the
 * command line.
 *
 * Unlike the rest of the corpus this is C, built with a compiler, startup
 * code and C library that DOSH's authors did not write (bcc and its DOS
 * libc). It is here to keep the compatibility contract honest: whatever a
 * real runtime asks of DOS, DOSH has to answer.
 */
#include <stdio.h>
#include <ctype.h>

static void count(FILE *f, char *name)
{
    long lines = 0, words = 0, bytes = 0;
    int c, inword = 0;

    while ((c = getc(f)) != EOF) {
        bytes++;
        if (c == '\n')
            lines++;
        if (isspace(c))
            inword = 0;
        else if (!inword) {
            inword = 1;
            words++;
        }
    }
    printf("%7ld %7ld %7ld %s\n", lines, words, bytes, name);
}

int main(int argc, char **argv)
{
    int i, status = 0;

    for (i = 1; i < argc; i++) {
        FILE *f = fopen(argv[i], "r");
        if (f == NULL) {
            fprintf(stderr, "wc: cannot open %s\n", argv[i]);
            status = 1;
            continue;
        }
        count(f, argv[i]);
        fclose(f);
    }
    return status;
}
