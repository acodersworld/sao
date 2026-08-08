#ifndef SAO_FUNCTION_H
#define SAO_FUNCTION_H

#include "sao_task.h"
#include "sao_value.h"

typedef enum SaoStepStatus {
    SAO_STEP_CALL,
    SAO_STEP_YIELD,
    SAO_STEP_RETURN,
} SaoStepStatus;

typedef struct SaoStepResult {
    SaoStepStatus status;
    SaoValue value;
} SaoStepResult;

typedef SaoStepResult (*SaoFunction)(
    SaoTask *task,
    void *frame,
    SaoValue previous
);

#endif
