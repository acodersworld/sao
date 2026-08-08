#include "sao_test.h"
#include "sao_task.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

#if defined(_MSC_VER)
#define TEST_FRAME_ALIGNMENT _Alignof(long double)
#else
#define TEST_FRAME_ALIGNMENT _Alignof(max_align_t)
#endif

typedef struct TestChildFrame {
    int value;
} TestChildFrame;

typedef struct TestRootFrame {
    int state;
    int *result;
    TestChildFrame child;
} TestRootFrame;

typedef struct TestYieldFrame {
    int state;
} TestYieldFrame;

static void test_value_constructors(void)
{
    SaoValue unit = sao_value_unit();
    SaoValue integer = sao_value_int(-42);
    SaoValue floating = sao_value_float(1.5);
    SaoValue byte = sao_value_byte(255);
    SaoValue object = sao_value_object(NULL);

    assert(unit.tag == SAO_VALUE_UNIT);
    assert(integer.tag == SAO_VALUE_INT);
    assert(integer.as_int == -42);
    assert(floating.tag == SAO_VALUE_FLOAT);
    assert(floating.as_float == 1.5);
    assert(byte.tag == SAO_VALUE_BYTE);
    assert(byte.as_byte == 255);
    assert(object.tag == SAO_VALUE_OBJECT);
    assert(object.as_obj == NULL);
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

    assert(scheduler == NULL);
    assert((uintptr_t) raw_frame % TEST_FRAME_ALIGNMENT == 0);
    TestChildFrame *frame = raw_frame;

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_int(frame->value),
    };
}

static SaoFunctionResult root_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *raw_frame,
    SaoValue previous
)
{
    assert(scheduler == NULL);
    assert((uintptr_t) raw_frame % TEST_FRAME_ALIGNMENT == 0);
    TestRootFrame *frame = raw_frame;

    if (frame->state == 0) {
        // The task-owned root frame creates a child that returns 41.
        frame->state = 1;
        frame->child.value = 41;

        assert(sao_task_push_function(
            task,
            child_function,
            &frame->child,
            sizeof(frame->child)
        ));

        return (SaoFunctionResult) {
            .status = SAO_FUNCTION_CALL,
            .value = sao_value_unit(),
        };
    }

    if (frame->state == 1) {
        // The root resumes with the child's result and exposes 41 + 1 to
        // the test through the shallow-copied result pointer.
        assert(previous.tag == SAO_VALUE_INT);
        *frame->result = (int) previous.as_int + 1;
        frame->state = 2;

        return (SaoFunctionResult) {
            .status = SAO_FUNCTION_YIELD,
            .value = sao_value_unit(),
        };
    }

    assert(frame->state == 2);

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_int(*frame->result),
    };
}

static SaoFunctionResult empty_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert(scheduler == NULL);
    assert((uintptr_t) frame % TEST_FRAME_ALIGNMENT == 0);

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_unit(),
    };
}

static SaoFunctionResult invalid_status_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *frame,
    SaoValue previous
)
{
    (void) task;
    (void) frame;
    (void) previous;

    assert(scheduler == NULL);

    return (SaoFunctionResult) {
        .status = (SaoFunctionStatus) -1,
        .value = sao_value_unit(),
    };
}

static size_t test_frame_record_size(size_t frame_size)
{
    const size_t alignment = TEST_FRAME_ALIGNMENT;
    size_t size = frame_size + sizeof(size_t);
    size_t remainder = size % alignment;

    if (remainder != 0) {
        size += alignment - remainder;
    }

    return size;
}

static SaoFunctionResult yielding_function(
    SaoTask *task,
    SaoScheduler *scheduler,
    void *raw_frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert(scheduler == NULL);
    assert((uintptr_t) raw_frame % TEST_FRAME_ALIGNMENT == 0);
    TestYieldFrame *frame = raw_frame;

    if (frame->state == 0) {
        frame->state = 1;

        return (SaoFunctionResult) {
            .status = SAO_FUNCTION_YIELD,
            .value = sao_value_unit(),
        };
    }

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_int(7),
    };
}

static void crash_frame_overflow(void)
{
    SaoTask task;
    unsigned char frame = 0;

    assert(sao_task_init(&task, 1));
    (void) sao_task_push_function(
        &task,
        empty_function,
        &frame,
        SIZE_MAX
    );
    sao_task_deinit(&task);
}

static void crash_invalid_status(void)
{
    SaoTask task;

    assert(sao_task_init(&task, 0));
    assert(sao_task_push_function(
        &task,
        invalid_status_function,
        NULL,
        0
    ));
    (void) sao_task_run(&task, NULL);
    sao_task_deinit(&task);
}

static void test_frame_overflow_crashes(void)
{
    sao_test_assert_crashes(
        crash_frame_overflow,
        "sao: task frame size overflow\n"
    );
}

static void test_invalid_status_crashes(void)
{
    sao_test_assert_crashes(
        crash_invalid_status,
        "sao: invalid function status\n"
    );
}

static void test_call_and_return(void)
{
    SaoTask task;
    int result = 0;

    // The frame owns its execution state, but points to an external result
    // variable so the test can observe work performed by the copied frame.
    TestRootFrame frame = {
        .result = &result,
    };

    assert(sao_task_init(&task, 0));

    // Push copies the complete frame, including state == 0 and the result
    // pointer, into the task-owned byte stack.
    assert(sao_task_push_function(
        &task,
        root_function,
        &frame,
        sizeof(frame)
    ));

    size_t root_frame_top = task.frame_top;

    // This changes only the source frame. If the task retained &frame instead
    // of copying it, root_function would see state == 99 and fail.
    frame.state = 99;

    // The copied root frame sees state == 0, calls the child, receives 41,
    // writes 42 through its copied result pointer, and then yields.
    SaoTaskStatus yielded = sao_task_run(&task, NULL);

    assert(yielded == SAO_TASK_RUNNING);
    assert(result == 42);
    assert(task.depth == 1);

    // The child has returned and been popped, leaving exactly the original
    // root-frame record on the task's byte stack.
    assert(task.frame_top == root_frame_top);

    // Resuming lets the root return and empties both task stacks.
    SaoTaskStatus status = sao_task_run(&task, NULL);

    assert(status == SAO_TASK_FINISHED);
    assert(result == 42);
    assert(task.depth == 0);
    assert(task.frame_top == 0);

    sao_task_deinit(&task);
}

static void test_yield_and_resume(void)
{
    SaoTask task;
    TestYieldFrame frame = {0};

    assert(sao_task_init(&task, 0));
    assert(sao_task_push_function(
        &task,
        yielding_function,
        &frame,
        sizeof(frame)
    ));

    frame.state = 99;

    SaoTaskStatus yielded = sao_task_run(&task, NULL);
    assert(yielded == SAO_TASK_RUNNING);
    assert(task.depth == 1);

    SaoTaskStatus returned = sao_task_run(&task, NULL);
    assert(returned == SAO_TASK_FINISHED);
    assert(task.depth == 0);

    sao_task_deinit(&task);
}

static void test_empty_task(void)
{
    SaoTask task;

    assert(sao_task_init(&task, 0));
    assert(task.frame_capacity == SAO_TASK_DEFAULT_FRAME_CAPACITY);
    assert(task.frame_top == 0);
    assert(task.depth == 0);
    assert(sao_task_run(&task, NULL) == SAO_TASK_FINISHED);

    sao_task_deinit(&task);
    assert(task.frame_stack == NULL);
    assert(task.frame_capacity == 0);
    assert(task.frame_top == 0);
    assert(task.depth == 0);
}

static void test_frame_capacity(void)
{
    SaoTask task;
    size_t capacity = test_frame_record_size(0);

    assert(sao_task_init(&task, capacity));
    assert(task.frame_capacity == capacity);
    assert(sao_task_push_function(&task, empty_function, NULL, 0));
    assert(task.frame_top == capacity);

    size_t depth = task.depth;
    size_t frame_top = task.frame_top;

    assert(!sao_task_push_function(&task, empty_function, NULL, 0));
    assert(!sao_task_push_function(&task, empty_function, NULL, 1));
    assert(task.depth == depth);
    assert(task.frame_top == frame_top);

    assert(sao_task_run(&task, NULL) == SAO_TASK_FINISHED);
    assert(task.frame_top == 0);
    sao_task_deinit(&task);
}

static void test_stack_capacity(void)
{
    SaoTask task;
    TestYieldFrame frame = {0};
    size_t frame_capacity =
        test_frame_record_size(sizeof(frame)) * SAO_TASK_STACK_CAPACITY;

    assert(sao_task_init(&task, frame_capacity));

    for (size_t index = 0; index < SAO_TASK_STACK_CAPACITY; index += 1) {
        assert(sao_task_push_function(
            &task,
            yielding_function,
            &frame,
            sizeof(frame)
        ));
    }

    assert(task.depth == SAO_TASK_STACK_CAPACITY);
    size_t frame_top = task.frame_top;
    assert(!sao_task_push_function(
        &task,
        yielding_function,
        &frame,
        sizeof(frame)
    ));
    assert(task.depth == SAO_TASK_STACK_CAPACITY);
    assert(task.frame_top == frame_top);

    sao_task_deinit(&task);
    assert(task.depth == 0);
    assert(task.frame_top == 0);
}

int main(void)
{
    sao_test_init();

    ADD_TEST(test_value_constructors);
    ADD_TEST(test_frame_overflow_crashes);
    ADD_TEST(test_invalid_status_crashes);
    ADD_TEST(test_empty_task);
    ADD_TEST(test_call_and_return);
    ADD_TEST(test_yield_and_resume);
    ADD_TEST(test_frame_capacity);
    ADD_TEST(test_stack_capacity);

    return sao_test_run_all();
}
