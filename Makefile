CC ?= gcc

CPPFLAGS := -Isrc/include
CFLAGS ?= -std=c11 -Wall -Wextra -Werror

BUILD_DIR := build
LIST_TEST_BINARY := $(BUILD_DIR)/sao_list_test
SCHEDULER_TEST_BINARY := $(BUILD_DIR)/sao_scheduler_test
SCHEDULER_TASK_TEST_BINARY := $(BUILD_DIR)/sao_scheduler_task_test
TASK_TEST_BINARY := $(BUILD_DIR)/sao_task_test
TEST_BINARIES := \
	$(LIST_TEST_BINARY) \
	$(SCHEDULER_TEST_BINARY) \
	$(SCHEDULER_TASK_TEST_BINARY) \
	$(TASK_TEST_BINARY)

LIST_TEST_SOURCES := \
	src/src/sao_list.c \
	tests/sao_list_test.c
SCHEDULER_TEST_SOURCES := \
	src/src/sao_list.c \
	src/src/sao_scheduler.c \
	src/src/sao_task.c \
	src/src/sao_value.c \
	tests/sao_test.c \
	tests/sao_scheduler_test.c
SCHEDULER_TASK_TEST_SOURCES := \
	src/src/sao_list.c \
	src/src/sao_scheduler.c \
	src/src/sao_task.c \
	src/src/sao_value.c \
	tests/sao_test.c \
	tests/sao_scheduler_task_test.c
TASK_TEST_SOURCES := \
	src/src/sao_list.c \
	src/src/sao_task.c \
	src/src/sao_value.c \
	tests/sao_test.c \
	tests/sao_task_test.c
HEADERS := $(wildcard src/include/*.h) $(wildcard tests/*.h)

.PHONY: all test clean

all: test

$(LIST_TEST_BINARY): $(LIST_TEST_SOURCES) $(HEADERS) | $(BUILD_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(LIST_TEST_SOURCES) -o $(LIST_TEST_BINARY)

$(SCHEDULER_TEST_BINARY): $(SCHEDULER_TEST_SOURCES) $(HEADERS) | $(BUILD_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(SCHEDULER_TEST_SOURCES) -o $(SCHEDULER_TEST_BINARY)

$(SCHEDULER_TASK_TEST_BINARY): $(SCHEDULER_TASK_TEST_SOURCES) $(HEADERS) | $(BUILD_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(SCHEDULER_TASK_TEST_SOURCES) -o $(SCHEDULER_TASK_TEST_BINARY)

$(TASK_TEST_BINARY): $(TASK_TEST_SOURCES) $(HEADERS) | $(BUILD_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(TASK_TEST_SOURCES) -o $(TASK_TEST_BINARY)

$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

test: $(TEST_BINARIES)
	./$(LIST_TEST_BINARY)
	./$(SCHEDULER_TEST_BINARY)
	./$(SCHEDULER_TASK_TEST_BINARY)
	./$(TASK_TEST_BINARY)

clean:
	$(RM) $(TEST_BINARIES)
