#define _POSIX_C_SOURCE 200809L

#include "sao_test.h"

#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static SaoList tests;

void sao_test_init(void)
{
    sao_list_init(&tests);
}

void sao_test_add(SaoTest *test)
{
    assert(test != NULL);

    sao_list_link_init(&test->link);
    sao_list_push_back(&tests, &test->link);
}

void sao_test_assert_crashes(
    SaoTestFunction function,
    const char *expected_error
)
{
    assert(function != NULL);
    assert(expected_error != NULL);

    int error_pipe[2];
    assert(pipe(error_pipe) == 0);

    pid_t child = fork();
    assert(child >= 0);

    if (child == 0) {
        assert(close(error_pipe[0]) == 0);
        assert(dup2(error_pipe[1], STDERR_FILENO) == STDERR_FILENO);
        assert(close(error_pipe[1]) == 0);

        function();
        _exit(EXIT_SUCCESS);
    }

    assert(close(error_pipe[1]) == 0);

    char actual_error[256];
    size_t actual_size = 0;

    while (actual_size < sizeof(actual_error) - 1) {
        ssize_t read_size = read(
            error_pipe[0],
            actual_error + actual_size,
            sizeof(actual_error) - 1 - actual_size
        );

        if (read_size > 0) {
            actual_size += (size_t) read_size;
            continue;
        }

        if (read_size == -1 && errno == EINTR) {
            continue;
        }

        assert(read_size == 0);
        break;
    }

    actual_error[actual_size] = '\0';
    assert(close(error_pipe[0]) == 0);

    int status;
    pid_t waited;

    do {
        waited = waitpid(child, &status, 0);
    } while (waited == -1 && errno == EINTR);

    assert(waited == child);
    assert(WIFEXITED(status));
    assert(WEXITSTATUS(status) == EXIT_FAILURE);
    assert(strcmp(actual_error, expected_error) == 0);
}

int sao_test_run_all(void)
{
    size_t test_count = tests.size;
    SaoListLink *link;

    while ((link = sao_list_pop_front(&tests)) != NULL) {
        SaoTest *test = (SaoTest *) link;
        printf("[ RUN  ] %s\n", test->name);
        fflush(stdout);
        test->function();
        printf("[ PASS ] %s\n", test->name);
    }

    printf("[ PASS ] all %zu tests\n", test_count);
    return 0;
}
