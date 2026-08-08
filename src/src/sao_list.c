#include "sao_list.h"

#include <assert.h>
#include <stddef.h>

void sao_list_init(SaoList *list)
{
    assert(list != NULL);

    list->head.next = &list->tail;
    list->head.prev = &list->head;
    list->tail.next = &list->tail;
    list->tail.prev = &list->head;
    list->size = 0;
}

void sao_list_link_init(SaoListLink *link)
{
    assert(link != NULL);

    link->next = link;
    link->prev = link;
}

bool sao_list_is_empty(const SaoList *list)
{
    assert(list != NULL);

    return list->head.next == &list->tail;
}

void sao_list_push_back(SaoList *list, SaoListLink *link)
{
    assert(list != NULL);
    assert(link != NULL);
    assert(link->next == link);
    assert(link->prev == link);

    SaoListLink *previous = list->tail.prev;

    link->next = &list->tail;
    link->prev = previous;
    previous->next = link;
    list->tail.prev = link;
    list->size += 1;
}

SaoListLink *sao_list_pop_front(SaoList *list)
{
    assert(list != NULL);

    SaoListLink *link = list->head.next;

    if (link == &list->tail) {
        assert(list->size == 0);
        return NULL;
    }

    assert(list->size > 0);

    link->prev->next = link->next;
    link->next->prev = link->prev;
    sao_list_link_init(link);
    list->size -= 1;
    return link;
}
