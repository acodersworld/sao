#ifndef SAO_FUNCTION_H
#define SAO_FUNCTION_H

#include "sao_value.h"

typedef struct SaoTask SaoTask;

typedef enum SaoFunctionStatus {
    SAO_FUNCTION_CALL,
    SAO_FUNCTION_YIELD,
    SAO_FUNCTION_RETURN,
} SaoFunctionStatus;

typedef struct SaoFunctionResult {
    SaoFunctionStatus status;
    SaoValue value;
} SaoFunctionResult;

typedef SaoFunctionResult (*SaoFunction)(
    SaoTask *task,
    void *frame,
    SaoValue previous
);

#endif
