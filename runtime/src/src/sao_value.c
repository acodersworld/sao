#include "sao_value.h"

SaoValue sao_value_unit(void)
{
    return (SaoValue) {
        .tag = SAO_VALUE_UNIT,
    };
}

SaoValue sao_value_int(int64_t value)
{
    return (SaoValue) {
        .tag = SAO_VALUE_INT,
        .as_int = value,
    };
}

SaoValue sao_value_float(double value)
{
    return (SaoValue) {
        .tag = SAO_VALUE_FLOAT,
        .as_float = value,
    };
}

SaoValue sao_value_byte(uint8_t value)
{
    return (SaoValue) {
        .tag = SAO_VALUE_BYTE,
        .as_byte = value,
    };
}

SaoValue sao_value_object(SaoObject *value)
{
    return (SaoValue) {
        .tag = SAO_VALUE_OBJECT,
        .as_obj = value,
    };
}
