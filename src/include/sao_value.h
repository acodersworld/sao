#ifndef SAO_VALUE_H
#define SAO_VALUE_H

#include <stdint.h>

#include "sao_object.h"

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

SaoValue sao_value_unit(void);

SaoValue sao_value_int(int64_t value);

SaoValue sao_value_float(double value);

SaoValue sao_value_byte(uint8_t value);

SaoValue sao_value_object(SaoObject *value);

#endif
