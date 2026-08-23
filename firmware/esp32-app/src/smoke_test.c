/* Tiny C smoke test for ESP32-C61 - just prints "Hello" */
#include <stdio.h>
#include <esp_system.h>
#include <esp_cpu.h>

void app_main(void) {
    printf("Hello from C smoke test!\n");
    fflush(stdout);
    /* Make sure we boot successfully */
    for (volatile int i = 0; i < 100000; i++) {
        /* spin */
    }
    printf("mAgent boot OK\n");
    fflush(stdout);
}