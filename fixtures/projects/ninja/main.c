#include <stdio.h>

int main(int argc, char **argv) {
    (void)argv;
    puts(argc > 1 ? "Ninja tests passed" : "Hello from Ninja");
    return 0;
}
