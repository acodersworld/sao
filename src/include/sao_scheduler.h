#ifndef SAO_SCHEDULER_H
#define SAO_SCHEDULER_H

#include <stdbool.h>
#include <stddef.h>

#include "sao_list.h"
#include "sao_task.h"

typedef struct SaoSchedulerTask SaoSchedulerTask;

typedef struct SaoScheduler {
    SaoList ready;
    SaoSchedulerTask *main_task;
} SaoScheduler;

void sao_scheduler_init(SaoScheduler *scheduler);

bool sao_scheduler_push_task(
    SaoScheduler *scheduler,
    SaoFunction function,
    const void *frame,
    size_t frame_size,
    size_t frame_capacity
);

void sao_scheduler_run(SaoScheduler *scheduler);

void sao_scheduler_deinit(SaoScheduler *scheduler);

#endif
