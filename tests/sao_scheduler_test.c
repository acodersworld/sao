#include "sao_scheduler.h"
#include "sao_test.h"

#include <assert.h>

typedef struct SequenceFrame {
    int id;
    int step;
    int step_count;
    int *events;
    size_t *event_count;
} SequenceFrame;

static SaoFunctionResult sequence_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *raw_frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert(scheduler != NULL);
    SequenceFrame *frame = raw_frame;
    frame->step += 1;
    frame->events[*frame->event_count] = frame->id * 10 + frame->step;
    *frame->event_count += 1;

    return (SaoFunctionResult) {
        .status = frame->step < frame->step_count
            ? SAO_FUNCTION_YIELD
            : SAO_FUNCTION_RETURN,
        .value = sao_value_unit(),
    };
}

static bool push_sequence(
    SaoScheduler *scheduler,
    int id,
    int step_count,
    int *events,
    size_t *event_count
)
{
    SequenceFrame frame = {
        .id = id,
        .step_count = step_count,
        .events = events,
        .event_count = event_count,
    };

    return sao_scheduler_push_task(
        scheduler,
        sequence_function,
        &frame,
        sizeof(frame),
        0
    );
}

static void assert_events(
    const int *actual,
    size_t actual_count,
    const int *expected,
    size_t expected_count
)
{
    assert(actual_count == expected_count);

    for (size_t index = 0; index < expected_count; index += 1) {
        assert(actual[index] == expected[index]);
    }
}

static void test_empty_scheduler(void)
{
    SaoScheduler scheduler;
    sao_scheduler_init(&scheduler);

    assert(scheduler.main_task == NULL);
    assert(scheduler.current_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));

    sao_scheduler_run(&scheduler);
    sao_scheduler_deinit(&scheduler);

    assert(scheduler.main_task == NULL);
    assert(scheduler.current_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));
}

static void test_first_successful_task_is_main(void)
{
    SaoScheduler scheduler;
    int events[1] = {0};
    size_t event_count = 0;
    SequenceFrame frame = {
        .id = 1,
        .step_count = 1,
        .events = events,
        .event_count = &event_count,
    };
    sao_scheduler_init(&scheduler);

    assert(!sao_scheduler_push_task(
        &scheduler,
        sequence_function,
        &frame,
        sizeof(frame),
        1
    ));
    assert(scheduler.main_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));

    assert(push_sequence(&scheduler, 1, 1, events, &event_count));
    assert(scheduler.main_task != NULL);
    assert(!sao_list_is_empty(&scheduler.ready));

    sao_scheduler_run(&scheduler);

    const int expected[] = {11};
    assert_events(events, event_count, expected, 1);
    assert(scheduler.main_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));
    sao_scheduler_deinit(&scheduler);
}

static void test_fifo_round_robin(void)
{
    SaoScheduler scheduler;
    int events[5] = {0};
    size_t event_count = 0;
    sao_scheduler_init(&scheduler);

    assert(push_sequence(&scheduler, 1, 3, events, &event_count));
    assert(push_sequence(&scheduler, 2, 2, events, &event_count));
    sao_scheduler_run(&scheduler);

    const int expected[] = {11, 21, 12, 22, 13};
    assert_events(events, event_count, expected, 5);
    sao_scheduler_deinit(&scheduler);
}

static void test_non_main_completion(void)
{
    SaoScheduler scheduler;
    int events[4] = {0};
    size_t event_count = 0;
    sao_scheduler_init(&scheduler);

    assert(push_sequence(&scheduler, 1, 3, events, &event_count));
    assert(push_sequence(&scheduler, 2, 1, events, &event_count));
    sao_scheduler_run(&scheduler);

    const int expected[] = {11, 21, 12, 13};
    assert_events(events, event_count, expected, 4);
    sao_scheduler_deinit(&scheduler);
}

static void test_main_completion_abandons_tasks(void)
{
    SaoScheduler scheduler;
    int events[3] = {0};
    size_t event_count = 0;
    sao_scheduler_init(&scheduler);

    assert(push_sequence(&scheduler, 1, 2, events, &event_count));
    assert(push_sequence(&scheduler, 2, 3, events, &event_count));
    sao_scheduler_run(&scheduler);

    const int expected[] = {11, 21, 12};
    assert_events(events, event_count, expected, 3);
    assert(scheduler.main_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));
    sao_scheduler_deinit(&scheduler);
}

static void test_sole_task_resumes_immediately(void)
{
    SaoScheduler scheduler;
    int events[3] = {0};
    size_t event_count = 0;
    sao_scheduler_init(&scheduler);

    assert(push_sequence(&scheduler, 1, 3, events, &event_count));
    sao_scheduler_run(&scheduler);

    const int expected[] = {11, 12, 13};
    assert_events(events, event_count, expected, 3);
    sao_scheduler_deinit(&scheduler);
}

static void test_deinit_abandons_unrun_tasks(void)
{
    SaoScheduler scheduler;
    int events[2] = {0};
    size_t event_count = 0;
    sao_scheduler_init(&scheduler);

    assert(push_sequence(&scheduler, 1, 1, events, &event_count));
    assert(push_sequence(&scheduler, 2, 1, events, &event_count));
    sao_scheduler_deinit(&scheduler);

    assert(event_count == 0);
    assert(scheduler.main_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));
}

static void test_scheduler_reuse(void)
{
    SaoScheduler scheduler;
    int events[2] = {0};
    size_t event_count = 0;
    sao_scheduler_init(&scheduler);

    assert(push_sequence(&scheduler, 1, 1, events, &event_count));
    sao_scheduler_run(&scheduler);
    assert(scheduler.main_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));

    assert(push_sequence(&scheduler, 2, 1, events, &event_count));
    assert(scheduler.main_task != NULL);
    sao_scheduler_run(&scheduler);

    const int expected[] = {11, 21};
    assert_events(events, event_count, expected, 2);
    assert(scheduler.main_task == NULL);
    assert(sao_list_is_empty(&scheduler.ready));
    sao_scheduler_deinit(&scheduler);
}

int main(void)
{
    sao_test_init();

    ADD_TEST(test_empty_scheduler);
    ADD_TEST(test_first_successful_task_is_main);
    ADD_TEST(test_fifo_round_robin);
    ADD_TEST(test_non_main_completion);
    ADD_TEST(test_main_completion_abandons_tasks);
    ADD_TEST(test_sole_task_resumes_immediately);
    ADD_TEST(test_deinit_abandons_unrun_tasks);
    ADD_TEST(test_scheduler_reuse);

    return sao_test_run_all();
}
