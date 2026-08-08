#include "sao_scheduler.h"
#include "sao_test.h"

#include <assert.h>

typedef struct EventLog {
    int events[4];
    size_t size;
} EventLog;

typedef struct ChildFrame {
    EventLog *log;
} ChildFrame;

typedef struct ParentFrame {
    int state;
    EventLog *log;
} ParentFrame;

static void record_event(EventLog *log, int event)
{
    assert(log->size < sizeof(log->events) / sizeof(log->events[0]));
    log->events[log->size] = event;
    log->size += 1;
}

static SaoFunctionResult child_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *raw_frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert(scheduler != NULL);
    assert(scheduler->current_task != NULL);
    ChildFrame *frame = raw_frame;
    record_event(frame->log, 3);

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_unit(),
    };
}

static SaoFunctionResult parent_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *raw_frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert(scheduler != NULL);
    assert(scheduler->current_task != NULL);
    ParentFrame *frame = raw_frame;

    if (frame->state == 0) {
        record_event(frame->log, 1);

        ChildFrame child = {
            .log = frame->log,
        };
        assert(sao_scheduler_push_task(
            scheduler,
            child_function,
            &child,
            sizeof(child),
            0
        ));
        assert(scheduler->ready.size == 1);

        record_event(frame->log, 2);
        frame->state = 1;

        return (SaoFunctionResult) {
            .status = SAO_FUNCTION_YIELD,
            .value = sao_value_unit(),
        };
    }

    assert(frame->state == 1);
    record_event(frame->log, 4);

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_unit(),
    };
}

static void test_function_can_schedule_task(void)
{
    SaoScheduler scheduler;
    EventLog log = {0};
    ParentFrame parent = {
        .log = &log,
    };
    sao_scheduler_init(&scheduler);

    assert(sao_scheduler_push_task(
        &scheduler,
        parent_function,
        &parent,
        sizeof(parent),
        0
    ));

    sao_scheduler_run(&scheduler);

    const int expected[] = {1, 2, 3, 4};
    assert(log.size == sizeof(expected) / sizeof(expected[0]));

    for (size_t index = 0; index < log.size; index += 1) {
        assert(log.events[index] == expected[index]);
    }

    assert(scheduler.main_task == NULL);
    assert(scheduler.current_task == NULL);
    assert(scheduler.ready.size == 0);
    sao_scheduler_deinit(&scheduler);
}

int main(void)
{
    sao_test_init();
    ADD_TEST(test_function_can_schedule_task);
    return sao_test_run_all();
}
