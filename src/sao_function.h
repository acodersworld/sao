#ifndef SAO_FUNCTION_H
#define SAO_FUNCTION_H

#include <stdint.h>

typedef struct SaoObject SaoObject;
typedef struct SaoFrame SaoFrame;

typedef enum SaoValueTag {
    SAO_VALUE_UNIT,
    SAO_VALUE_INT,
    SAO_VALUE_FLOAT,
    SAO_VALUE_BYTE,
    SAO_VALUE_OBJECT,
} SaoValueTag;

typedef struct SaoValue {
    SaoValueTag tag;

    union {
        int64_t as_int;
        double as_float;
        uint8_t as_byte;
        SaoObject *as_obj;
    };
} SaoValue;

typedef enum SaoStepStatus {
    SAO_STEP_CALL,
    SAO_STEP_YIELD,
    SAO_STEP_RETURN,
} SaoStepStatus;

typedef struct SaoStepResult {
    SaoStepStatus status;
    SaoValue value;
} SaoStepResult;

typedef struct SaoTask {
    SaoFrame *top;
} SaoTask;

typedef SaoStepResult (*SaoFunction)(
    SaoTask *task,
    void *frame,
    SaoValue previous
);

#endif
