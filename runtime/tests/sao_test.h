#ifndef SAO_TEST_H
#define SAO_TEST_H

#include "sao_list.h"

typedef void (*SaoTestFunction)(void);

typedef struct SaoTest {
    SaoListLink link;
    const char *name;
    SaoTestFunction function;
} SaoTest;

void sao_test_init(void);

void sao_test_add(SaoTest *test);

void sao_test_assert_crashes(
    SaoTestFunction function,
    const char *expected_error
);

int sao_test_run_all(void);

#define ADD_TEST(test_function)                     \
    do {                                            \
        static SaoTest test = {                     \
            .name = #test_function,                 \
            .function = test_function,              \
        };                                          \
        sao_test_add(&test);                        \
    } while (0)

#endif
