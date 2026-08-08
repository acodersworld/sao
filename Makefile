CC ?= gcc

CPPFLAGS := -Isrc/include
CFLAGS ?= -std=c11 -Wall -Wextra -Werror

BUILD_DIR := build
TEST_BINARY := $(BUILD_DIR)/sao_task_test
TEST_SOURCES := \
	src/src/sao_task.c \
	src/src/sao_value.c \
	tests/sao_task_test.c
HEADERS := $(wildcard src/include/*.h)

.PHONY: all test clean

all: test

$(TEST_BINARY): $(TEST_SOURCES) $(HEADERS) | $(BUILD_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(TEST_SOURCES) -o $(TEST_BINARY)

$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

test: $(TEST_BINARY)
	./$(TEST_BINARY)

clean:
	$(RM) $(TEST_BINARY)
