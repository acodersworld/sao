#include "sao_list.h"
#include "sao_task.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

#if defined(_MSC_VER)
#define TEST_FRAME_ALIGNMENT _Alignof(long double)
#else
#define TEST_FRAME_ALIGNMENT _Alignof(max_align_t)
#endif

typedef void (*TestFunction)(void);

typedef struct Test {
    SaoListLink link;
    const char *name;
    TestFunction function;
} Test;

static SaoList tests;

static void add_test(Test *test)
{
    sao_list_link_init(&test->link);
    sao_list_push_back(&tests, &test->link);
}

#define ADD_TEST(test_function)                     \
    do {                                            \
        static Test test = {                        \
            .name = #test_function,                 \
            .function = test_function,              \
        };                                          \
        add_test(&test);                            \
    } while (0)

static int run_tests(void)
{
    size_t passed = 0;
    SaoListLink *link;

    while ((link = sao_list_pop_front(&tests)) != NULL) {
        Test *test = (Test *) link;
        printf("[ RUN  ] %s\n", test->name);
        fflush(stdout);
        test->function();
        printf("[ PASS ] %s\n", test->name);
        passed += 1;
    }

    printf("[ PASS ] all %zu tests\n", passed);
    return 0;
}

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
    void *raw_frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert((uintptr_t) raw_frame % TEST_FRAME_ALIGNMENT == 0);
    TestChildFrame *frame = raw_frame;

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
        .value = sao_value_int(frame->value),
    };
}

static SaoFunctionResult root_function(
    SaoTask *task,
    void *raw_frame,
    SaoValue previous
)
{
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
    void *frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

    assert((uintptr_t) frame % TEST_FRAME_ALIGNMENT == 0);

    return (SaoFunctionResult) {
        .status = SAO_FUNCTION_RETURN,
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
    void *raw_frame,
    SaoValue previous
)
{
    (void) task;
    (void) previous;

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
    SaoTaskStatus yielded = sao_task_run(&task);

    assert(yielded == SAO_TASK_RUNNING);
    assert(result == 42);
    assert(task.depth == 1);

    // The child has returned and been popped, leaving exactly the original
    // root-frame record on the task's byte stack.
    assert(task.frame_top == root_frame_top);

    // Resuming lets the root return and empties both task stacks.
    SaoTaskStatus status = sao_task_run(&task);

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

    SaoTaskStatus yielded = sao_task_run(&task);
    assert(yielded == SAO_TASK_RUNNING);
    assert(task.depth == 1);

    SaoTaskStatus returned = sao_task_run(&task);
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
    assert(sao_task_run(&task) == SAO_TASK_FINISHED);

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

    assert(sao_task_run(&task) == SAO_TASK_FINISHED);
    assert(task.frame_top == 0);
    sao_task_deinit(&task);
}

static void test_stack_capacity(void)
{
    SaoTask task;
    TestYieldFrame frame = {0};

    assert(sao_task_init(&task, 0));

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
    sao_list_init(&tests);

    ADD_TEST(test_value_constructors);
    ADD_TEST(test_empty_task);
    ADD_TEST(test_call_and_return);
    ADD_TEST(test_yield_and_resume);
    ADD_TEST(test_frame_capacity);
    ADD_TEST(test_stack_capacity);

    return run_tests();
}
