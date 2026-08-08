#ifndef SAO_LIST_H
#define SAO_LIST_H

#include <stdbool.h>

typedef struct SaoListLink {
    struct SaoListLink *next;
    struct SaoListLink *prev;
} SaoListLink;

typedef struct SaoList {
    SaoListLink head;
    SaoListLink tail;
} SaoList;

void sao_list_init(SaoList *list);

void sao_list_link_init(SaoListLink *link);

bool sao_list_is_empty(const SaoList *list);

void sao_list_push_back(SaoList *list, SaoListLink *link);

SaoListLink *sao_list_pop_front(SaoList *list);

void sao_list_remove(SaoListLink *link);

#endif
