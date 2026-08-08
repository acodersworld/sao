#include "sao_list.h"

#include <assert.h>
#include <stdio.h>

#define RUN_TEST(test)                     \
    do {                                   \
        printf("[ RUN  ] %s\n", #test);    \
        fflush(stdout);                    \
        test();                            \
        printf("[ PASS ] %s\n", #test);    \
    } while (0)

typedef struct TestNode {
    SaoListLink link;
    int value;
} TestNode;

static void init_node(TestNode *node, int value)
{
    sao_list_link_init(&node->link);
    node->value = value;
}

static TestNode *pop_node(SaoList *list)
{
    return (TestNode *) sao_list_pop_front(list);
}

static void push_three(
    SaoList *list,
    TestNode *first,
    TestNode *second,
    TestNode *third
)
{
    sao_list_init(list);
    init_node(first, 1);
    init_node(second, 2);
    init_node(third, 3);
    sao_list_push_back(list, &first->link);
    sao_list_push_back(list, &second->link);
    sao_list_push_back(list, &third->link);
}

static void test_empty_list(void)
{
    SaoList list;
    sao_list_init(&list);

    assert(list.head.next != NULL);
    assert(list.head.prev != NULL);
    assert(list.tail.next != NULL);
    assert(list.tail.prev != NULL);
    assert(list.head.next == &list.tail);
    assert(list.head.prev == &list.head);
    assert(list.tail.next == &list.tail);
    assert(list.tail.prev == &list.head);
    assert(sao_list_is_empty(&list));
    assert(sao_list_pop_front(&list) == NULL);
}

static void test_fifo_order(void)
{
    SaoList list;
    TestNode first;
    TestNode second;
    TestNode third;
    push_three(&list, &first, &second, &third);

    assert(pop_node(&list)->value == 1);
    assert(pop_node(&list)->value == 2);
    assert(pop_node(&list)->value == 3);
    assert(sao_list_is_empty(&list));
}

static void test_remove_first(void)
{
    SaoList list;
    TestNode first;
    TestNode second;
    TestNode third;
    push_three(&list, &first, &second, &third);

    sao_list_remove(&first.link);

    assert(pop_node(&list)->value == 2);
    assert(pop_node(&list)->value == 3);
    assert(sao_list_is_empty(&list));
}

static void test_remove_middle(void)
{
    SaoList list;
    TestNode first;
    TestNode second;
    TestNode third;
    push_three(&list, &first, &second, &third);

    sao_list_remove(&second.link);

    assert(pop_node(&list)->value == 1);
    assert(pop_node(&list)->value == 3);
    assert(sao_list_is_empty(&list));
}

static void test_remove_last(void)
{
    SaoList list;
    TestNode first;
    TestNode second;
    TestNode third;
    push_three(&list, &first, &second, &third);

    sao_list_remove(&third.link);

    assert(pop_node(&list)->value == 1);
    assert(pop_node(&list)->value == 2);
    assert(sao_list_is_empty(&list));
}

static void test_reuse_removed_link(void)
{
    SaoList list;
    TestNode node;
    sao_list_init(&list);
    init_node(&node, 7);

    sao_list_push_back(&list, &node.link);
    assert(pop_node(&list) == &node);
    assert(node.link.next == &node.link);
    assert(node.link.prev == &node.link);

    sao_list_push_back(&list, &node.link);
    assert(pop_node(&list) == &node);
    assert(sao_list_is_empty(&list));
}

int main(void)
{
    RUN_TEST(test_empty_list);
    RUN_TEST(test_fifo_order);
    RUN_TEST(test_remove_first);
    RUN_TEST(test_remove_middle);
    RUN_TEST(test_remove_last);
    RUN_TEST(test_reuse_removed_link);

    printf("[ PASS ] all 6 tests\n");
    return 0;
}
