#include "sao_scheduler.h"

#include "sao_crash.h"

#include <assert.h>
#include <stdlib.h>

struct SaoSchedulerTask {
    SaoListLink link;
    SaoTask task;
};

static void sao_scheduler_destroy_task(SaoSchedulerTask *scheduled_task)
{
    sao_task_deinit(&scheduled_task->task);
    free(scheduled_task);
}

static void sao_scheduler_clear_tasks(SaoScheduler *scheduler)
{
    if (scheduler->current_task != NULL) {
        sao_crash("cannot deinitialize a running scheduler");
    }

    SaoListLink *link;

    while ((link = sao_list_pop_front(&scheduler->ready)) != NULL) {
        SaoSchedulerTask *scheduled_task = (SaoSchedulerTask *) link;
        sao_scheduler_destroy_task(scheduled_task);
    }

    scheduler->main_task = NULL;
}

void sao_scheduler_init(SaoScheduler *scheduler)
{
    assert(scheduler != NULL);

    sao_list_init(&scheduler->ready);
    scheduler->main_task = NULL;
    scheduler->current_task = NULL;
}

bool sao_scheduler_push_task(
    SaoScheduler *scheduler,
    SaoFunction function,
    const void *frame,
    size_t frame_size,
    size_t frame_capacity
)
{
    assert(scheduler != NULL);
    assert(function != NULL);

    SaoSchedulerTask *scheduled_task = malloc(sizeof(*scheduled_task));

    if (scheduled_task == NULL) {
        return false;
    }

    sao_list_link_init(&scheduled_task->link);

    if (!sao_task_init(&scheduled_task->task, frame_capacity)) {
        free(scheduled_task);
        return false;
    }

    if (!sao_task_push_function(
            &scheduled_task->task,
            function,
            frame,
            frame_size
    )) {
        sao_scheduler_destroy_task(scheduled_task);
        return false;
    }

    sao_list_push_back(&scheduler->ready, &scheduled_task->link);

    if (scheduler->main_task == NULL) {
        scheduler->main_task = scheduled_task;
    }

    return true;
}

void sao_scheduler_run(SaoScheduler *scheduler)
{
    assert(scheduler != NULL);

    if (scheduler->current_task != NULL) {
        sao_crash("recursive scheduler run");
    }

    while (scheduler->main_task != NULL) {
        SaoListLink *link = sao_list_pop_front(&scheduler->ready);
        assert(link != NULL);

        SaoSchedulerTask *scheduled_task = (SaoSchedulerTask *) link;
        scheduler->current_task = scheduled_task;
        SaoTaskStatus status = sao_task_run(&scheduled_task->task, scheduler);
        scheduler->current_task = NULL;

        if (status == SAO_TASK_RUNNING) {
            sao_list_push_back(&scheduler->ready, &scheduled_task->link);
            continue;
        }

        bool is_main = scheduled_task == scheduler->main_task;
        sao_scheduler_destroy_task(scheduled_task);

        if (is_main) {
            sao_scheduler_clear_tasks(scheduler);
            return;
        }
    }
}

void sao_scheduler_deinit(SaoScheduler *scheduler)
{
    assert(scheduler != NULL);

    sao_scheduler_clear_tasks(scheduler);
    sao_list_init(&scheduler->ready);
}
