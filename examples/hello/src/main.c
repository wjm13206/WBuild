#include <stdio.h>

#include "core.h"
#include "util.h"

int main(void) {
    printf("%s from WBuild (core=%d)\n", greeting(), core_value());
    return 0;
}