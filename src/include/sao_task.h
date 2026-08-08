#ifndef SAO_TASK_H
#define SAO_TASK_H

#include <stdbool.h>
#include <stddef.h>

#include "sao_function.h"

enum {
    SAO_TASK_STACK_CAPACITY = 1024,
};

#define SAO_TASK_DEFAULT_FRAME_CAPACITY (8u * 1024u)

typedef enum SaoTaskStatus {
    SAO_TASK_RUNNING,
    SAO_TASK_FINISHED,
} SaoTaskStatus;

struct SaoTask {
    SaoFunction functions[SAO_TASK_STACK_CAPACITY];
    unsigned char *frame_stack;
    size_t frame_capacity;
    size_t frame_top;
    size_t depth;
};

bool sao_task_init(SaoTask *task, size_t frame_capacity);

void sao_task_deinit(SaoTask *task);

bool sao_task_push_function(
    SaoTask *task,
    SaoFunction function,
    const void *frame,
    size_t frame_size
);

SaoTaskStatus sao_task_run(SaoTask *task, SaoScheduler *scheduler);

#endif
