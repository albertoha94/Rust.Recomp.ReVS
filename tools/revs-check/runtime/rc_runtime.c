/* Runtime for recompiled code (game-independent; part of the repo):
 * indirect calls through the guest-address -> C-function table, and traps.
 * The table itself is generated (build/recomp/rc_table.c). */
#include "rc_cpu.h"

typedef struct RcEntry {
    uint32_t addr;
    RcFn fn;
} RcEntry;

extern const RcEntry rc_table[];
extern const uint32_t rc_table_len;

RcFn rc_lookup(uint32_t addr) {
    uint32_t lo = 0, hi = rc_table_len;
    addr &= ~1u; /* Thumb bit */
    while (lo < hi) {
        uint32_t mid = lo + (hi - lo) / 2;
        if (rc_table[mid].addr < addr) lo = mid + 1;
        else hi = mid;
    }
    return (lo < rc_table_len && rc_table[lo].addr == addr) ? rc_table[lo].fn : 0;
}

void rc_trap(Cpu *c, uint32_t addr, uint32_t why) {
    if (!c->halt) {
        c->halt = why;
        c->halt_addr = addr;
    }
}

void rc_call(Cpu *c, uint32_t target) {
    RcFn f = rc_lookup(target);
    if (f) f(c);
    else rc_miss(c, target);
}

uint32_t rc_table_size(void) { return rc_table_len; }

#ifdef RC_NATIVE
uint8_t rc_cov[0x200000];
#endif
