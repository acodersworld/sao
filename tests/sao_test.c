#include "sao_test.h"

#include <assert.h>
#include <stdio.h>

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
