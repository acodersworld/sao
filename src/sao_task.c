#include "sao_task.h"

#include <assert.h>

void sao_task_init(SaoTask *task, SaoFunction function, void *frame)
{
    assert(task != NULL);
    assert(function != NULL);

    task->stack[0] = (SaoFrame) {
        .function = function,
        .frame = frame,
    };
    task->depth = 1;
}

bool sao_task_push(SaoTask *task, SaoFunction function, void *frame)
{
    assert(task != NULL);
    assert(function != NULL);

    if (task->depth == SAO_TASK_STACK_CAPACITY) {
        return false;
    }

    task->stack[task->depth] = (SaoFrame) {
        .function = function,
        .frame = frame,
    };
    task->depth += 1;

    return true;
}

SaoTaskStatus sao_task_run(SaoTask *task)
{
    assert(task != NULL);

    SaoValue previous = sao_value_unit();

    while (task->depth > 0) {
        size_t depth = task->depth;
        SaoFrame current = task->stack[depth - 1];
        SaoFunctionResult result = current.function(task, current.frame, previous);

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
            task->depth -= 1;

            if (task->depth == 0) {
                return SAO_TASK_FINISHED;
            }

            previous = result.value;
            break;
        }
    }

    return SAO_TASK_FINISHED;
}
