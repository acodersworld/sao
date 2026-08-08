#ifndef SAO_TASK_H
#define SAO_TASK_H

#include <stdbool.h>
#include <stddef.h>

#include "sao_function.h"

enum {
    SAO_TASK_STACK_CAPACITY = 1024,
};

typedef enum SaoTaskStatus {
    SAO_TASK_RUNNING,
    SAO_TASK_FINISHED,
} SaoTaskStatus;

typedef struct SaoFrame {
    SaoFunction function;
    void *frame;
} SaoFrame;

struct SaoTask {
    SaoFrame stack[SAO_TASK_STACK_CAPACITY];
    size_t depth;
};

void sao_task_init(SaoTask *task, SaoFunction function, void *frame);

bool sao_task_push(SaoTask *task, SaoFunction function, void *frame);

SaoTaskStatus sao_task_run(SaoTask *task);

#endif
