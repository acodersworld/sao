#include "sao_task.h"

#include "sao_crash.h"

#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(_MSC_VER)
#define SAO_FRAME_ALIGNMENT _Alignof(long double)
#else
#define SAO_FRAME_ALIGNMENT _Alignof(max_align_t)
#endif

static size_t sao_frame_record_size(size_t frame_size)
{
    const size_t alignment = SAO_FRAME_ALIGNMENT;

    if (frame_size > SIZE_MAX - sizeof(size_t)) {
        sao_crash("task frame size overflow");
    }

    size_t size = frame_size + sizeof(size_t);
    size_t remainder = size % alignment;

    if (remainder != 0) {
        size_t padding = alignment - remainder;

        if (size > SIZE_MAX - padding) {
            sao_crash("task frame size overflow");
        }

        size += padding;
    }

    return size;
}

static size_t sao_task_top_record_size(const SaoTask *task)
{
    assert(task->depth > 0);
    assert(task->frame_top >= sizeof(size_t));

    size_t record_size;
    memcpy(
        &record_size,
        task->frame_stack + task->frame_top - sizeof(size_t),
        sizeof(record_size)
    );

    return record_size;
}

static void *sao_task_top_frame(SaoTask *task)
{
    size_t record_size = sao_task_top_record_size(task);
    return task->frame_stack + task->frame_top - record_size;
}

static void sao_task_pop_function(SaoTask *task)
{
    size_t record_size = sao_task_top_record_size(task);

    task->frame_top -= record_size;
    task->depth -= 1;
    task->functions[task->depth] = NULL;
}

bool sao_task_init(SaoTask *task, size_t frame_capacity)
{
    assert(task != NULL);

    if (frame_capacity == 0) {
        frame_capacity = SAO_TASK_DEFAULT_FRAME_CAPACITY;
    }

    unsigned char *frame_stack = malloc(frame_capacity);

    if (frame_stack == NULL) {
        return false;
    }

    *task = (SaoTask) {
        .frame_stack = frame_stack,
        .frame_capacity = frame_capacity,
    };

    return true;
}

void sao_task_deinit(SaoTask *task)
{
    assert(task != NULL);

    free(task->frame_stack);
    *task = (SaoTask) {0};
}

bool sao_task_push_function(
    SaoTask *task,
    SaoFunction function,
    const void *frame,
    size_t frame_size
)
{
    assert(task != NULL);
    assert(function != NULL);
    assert(task->frame_stack != NULL);

    if (task->depth == SAO_TASK_STACK_CAPACITY) {
        return false;
    }

    if (frame_size > 0 && frame == NULL) {
        return false;
    }

    size_t record_size = sao_frame_record_size(frame_size);

    if (record_size > task->frame_capacity - task->frame_top) {
        return false;
    }

    unsigned char *destination = task->frame_stack + task->frame_top;

    if (frame_size > 0) {
        memcpy(destination, frame, frame_size);
    }

    memcpy(
        destination + record_size - sizeof(size_t),
        &record_size,
        sizeof(record_size)
    );

    task->functions[task->depth] = function;
    task->frame_top += record_size;
    task->depth += 1;

    return true;
}

SaoTaskStatus sao_task_run(SaoTask *task, SaoScheduler *scheduler)
{
    assert(task != NULL);

    SaoValue previous = sao_value_unit();

    while (task->depth > 0) {
        size_t depth = task->depth;
        SaoFunction function = task->functions[depth - 1];
        void *frame = sao_task_top_frame(task);
        SaoFunctionResult result = function(task, scheduler, frame, previous);

        previous = sao_value_unit();

        switch (result.status) {
        case SAO_FUNCTION_CALL:
            assert(task->depth > depth);
            break;

        case SAO_FUNCTION_YIELD:
            assert(task->depth == depth);
            return SAO_TASK_RUNNING;

        case SAO_FUNCTION_RETURN:
            assert(task->depth == depth);
            sao_task_pop_function(task);

            if (task->depth == 0) {
                return SAO_TASK_FINISHED;
            }

            previous = result.value;
            break;

        default:
            sao_crash("invalid function status");
        }
    }

    return SAO_TASK_FINISHED;
}
