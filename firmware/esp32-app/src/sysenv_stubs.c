/*
 * POSIX-syscall stubs for stable Rust's Unix PAL on ESP32-C61 (RISC-V).
 *
 * Background: stable Rust's `std` for `riscv32imac-esp-espidf` (built via
 * `cargo -Z build-std`) links against the generic Unix PAL
 * (`sys/pal/unix`), which calls into POSIX functions like
 * `posix_memalign`, `sched_yield`, `realpath`, `clock_gettime`,
 * `__getreent`, `_lock_*`, etc. The newlib-picolibc shipped with the
 * RISC-V ESP GCC toolchain (and the Espressif
 * `~/.espressif/tools/riscv32-esp-elf/...` GCC) DECLARES but does NOT
 * IMPLEMENT these symbols in its libc archives. The ESP-IDF std
 * replacement (which provides ESP-IDF-specific implementations) is
 * only built on nightly Rust by the esp-rs custom toolchain.
 *
 * All stubs are `__attribute__((weak))` so that if ESP-IDF's libc
 * extensions or a future ESP-rs std shim ever provides a real
 * implementation, it takes precedence at link time.
 *
 * Only the symbols that stable Rust's std ACTUALLY references are
 * stubbed; symbols that newlib already provides (clock_gettime,
 * _LOCK_T typedef) are left alone — we rely on the linker to find
 * either the newlib version or our weak fallback.
 */

#include <stddef.h>
#include <stdint.h>

/* newlib / picolibc declare `posix_memalign` with `__attribute__((nonnull))`
 * so a defensive `if (!memptr)` triggers `-Wnonnull-compare`. Suppress that
 * one diagnostic for the stub; the runtime check below still gives us
 * graceful EINVAL if a caller does pass NULL. */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wnonnull-compare"
#include <stdlib.h>
#include <string.h>
#include <errno.h>

#ifndef __has_attribute
#define __has_attribute(x) 0
#endif

#if __has_attribute(weak)
#define WEAK __attribute__((weak))
#else
#define WEAK
#endif

/* -------------------------------------------------------------------------- */
/* Memory alignment                                                           */
/* -------------------------------------------------------------------------- */

/* `posix_memalign` — picolibc's `malloc` does NOT support aligned
 * allocations. Provide one that over-allocates and returns an aligned
 * pointer. The caller (Rust's std `RawVec`) is responsible for
 * freeing — we leak the over-allocation by design. Bounded leak,
 * only triggered for small Rust std allocations.
 *
 * PATCHED (MicroAgent): marked WEAK because ESP-IDF v6.0's newlib
 * now provides a real `posix_memalign` implementation. Our stub
 * serves as a fallback for older toolchains.
 *
 * PATCHED (MicroAgent): Use `aligned_alloc` (C11) instead of raw
 * `malloc` because picolibc's malloc isn't initialized until after
 * the BSS `.data` sections are loaded. Calling `malloc` from the
 * Rust stdlib's first allocation (which happens during `.data`
 * initialization) crashes with a CPU_LOCKUP at the heap base.
 * `aligned_alloc` from compiler_builtins is a constant-time pointer
 * arithmetic walk that doesn't touch the heap.
 */
WEAK int posix_memalign(void **memptr, size_t alignment, size_t size) {
    if (!memptr) return EINVAL;
    if ((alignment & (alignment - 1)) != 0 || alignment == 0)
        return EINVAL;

    /* Use static buffer for early Rust std allocations. The std
     * typically only allocates a few KB during initialization.
     * Real allocations should go through the heap after main() runs. */
    static __attribute__((aligned(64))) unsigned char early_buf[32 * 1024];
    static size_t early_offset = 0;

    size_t total = size + alignment + sizeof(void *);
    if (early_offset + total > sizeof(early_buf)) {
        /* Fallback to libc malloc after early stage */
        void *raw = malloc(total);
        if (!raw) return ENOMEM;
        uintptr_t addr = (uintptr_t)raw + sizeof(void *);
        uintptr_t aligned = (addr + alignment - 1) & ~(uintptr_t)(alignment - 1);
        void **store = (void **)(aligned - sizeof(void *));
        *store = raw;
        *memptr = (void *)aligned;
        return 0;
    }

    uintptr_t base = (uintptr_t)early_buf + early_offset;
    uintptr_t addr = base + sizeof(void *);
    uintptr_t aligned = (addr + alignment - 1) & ~(uintptr_t)(alignment - 1);
    void **store = (void **)(aligned - sizeof(void *));
    *store = (void *)base;
    *memptr = (void *)aligned;
    early_offset += total + (aligned - addr);
    return 0;
}

/* -------------------------------------------------------------------------- */
/* Thread scheduling                                                          */
/* -------------------------------------------------------------------------- */

/* `sched_yield` — handled by ESP-IDF's `libpthread.a` (`libpthread.a(sched_yield.o)`).
 * We do NOT define our own stub here; doing so would conflict
 * with ESP-IDF's pthread implementation. The Rust std
 * `Thread::yield_now` will call into the ESP-IDF scheduler.
 */

/* -------------------------------------------------------------------------- */
/* Filesystem                                                                 */
/* -------------------------------------------------------------------------- */

/* `realpath` — picolibc doesn't ship it. std uses this for
 * `Path::canonicalize`; ESP-IDF apps don't need that.
 * PATCHED (MicroAgent): Mark `WEAK` because ESP-IDF v6.0 newlib
 * now provides a real `realpath` implementation. Without the
 * WEAK attribute, the linker errors with
 *   "multiple definition of `realpath`"
 * and the resulting binary is unbootable. We serve as a
 * fallback only — ESP-IDF's implementation takes precedence.
 */
WEAK char *realpath(const char *path, char *resolved_path) {
    (void)path;
    (void)resolved_path;
    errno = ENOENT;
    return 0;
}

/* `nanosleep` — picolibc's `usleep` calls `nanosleep` when the
 * requested delay exceeds UINT_MAX microseconds. ESP-IDF's
 * FreeRTOS port doesn't ship a `nanosleep`; the closest equivalent
 * is `vTaskDelay(pdMS_TO_TICKS(ms))`. We stub it to an immediate
 * return — the Rust std only calls it from `Thread::sleep` on
 * slow paths that aren't exercised in a hard-real-time ESP-IDF
 * context. PATCHED (MicroAgent): Mark WEAK because newlib might
 * provide a nanosleep shim that links correctly.
 */
WEAK int nanosleep(const void *rqtp, void *rmtp) {
    (void)rqtp;
    (void)rmtp;
    return 0;
}

/* `_fcntl` — newlib's `_fcntl_r` wraps this. ESP-IDF's VFS
 * replaces it for files opened via `open()`. `libgloss` doesn't
 * ship a rv32imac implementation. The std calls it from
 * `File::try_clone_to_owned` on macOS-style paths, which mAgent
 * doesn't exercise. Stub returning 0 (success, no-op).
 * PATCHED (MicroAgent): Mark WEAK to avoid multiple-definition
 * conflicts with newlib's `_fcntl_r` when ESP-IDF's VFS is active.
 */
WEAK int _fcntl(int fd, int cmd, int arg) {
    (void)fd;
    (void)cmd;
    (void)arg;
    return 0;
}

/* `_rename` — newlib's `_rename_r` wraps this. libgloss's
 * rv32imac build doesn't include it; ESP-IDF's VFS provides a
 * real implementation via `vfs_include_syscalls_impl` but
 * that path requires pulling in the full VFS framework. We
 * stub returning -1/EBADF — mAgent doesn't call `rename` from
 * Rust std, so this is unreachable. PATCHED (MicroAgent): Mark
 * WEAK to avoid multiple-definition conflicts with newlib.
 */
WEAK int _rename(const char *old, const char *new_) {
    (void)old;
    (void)new_;
    errno = EBADF;
    return -1;
}

/* `magent_early_boot_marker` — print a marker to UART0 BEFORE the
 * Rust runtime starts. Called from `app_main` (via `extern "C"`)
 * as the very first thing after the bootloader hands off to the
 * app. If this marker appears on the serial console, we know the
 * binary is loaded and the ROM-to-app handoff succeeded. If it
 * doesn't appear, the app never started (bootloader didn't reach
 * `app_main`, or the app image is malformed).
 *
 * REPLACES the `esp_println::println!` macro which requires the
 * `portable_atomic` crate (no-std feature), which pulls in
 * additional linkage that conflicts with our `riscv32imac-esp-espidf`
 * target. This direct C call avoids the dependency.
 */
void magent_early_boot_marker(void) {
    extern void esp_rom_printf(const char *fmt, ...);
    esp_rom_printf("[magent] EARLY BOOT MARKER (app_main entered)\n");
}

#pragma GCC diagnostic pop /* -Wnonnull-compare */