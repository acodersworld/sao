#include "sao_crash.h"

#include <stdio.h>
#include <stdlib.h>

_Noreturn void sao_crash(const char *message)
{
    fprintf(stderr, "sao: %s\n", message);
    exit(EXIT_FAILURE);
}
