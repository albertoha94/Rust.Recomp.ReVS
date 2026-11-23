/* Runtime for recompiled code (game-independent; part of the repo):
 * indirect calls through the guest-address -> C-function table, and traps.
 * The table itself is generated (build/recomp/rc_table.c).
 *
 * RC_DLL: the game is built as its own library (rc_game.dll / .so) that the
 * prebuilt runner loads at start, so a release never contains game code. The
 * host's three functions then come in through rc_dll_init. */
#include "rc_cpu.h"

typedef struct RcEntry {
    uint32_t addr;
    RcFn fn;
} RcEntry;

extern const RcEntry rc_table[];
extern const uint32_t rc_table_len;

RC_API RcFn rc_lookup(uint32_t addr) {
    uint32_t lo = 0, hi = rc_table_len;
    addr &= ~1u; /* Thumb bit */
    while (lo < hi) {
        uint32_t mid = lo + (hi - lo) / 2;
        if (rc_table[mid].addr < addr) lo = mid + 1;
        else hi = mid;
    }
    return (lo < rc_table_len && rc_table[lo].addr == addr) ? rc_table[lo].fn : 0;
}

RC_API void rc_trap(Cpu *c, uint32_t addr, uint32_t why) {
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

RC_API uint32_t rc_table_size(void) { return rc_table_len; }

#ifdef RC_NATIVE
uint8_t rc_cov[0x200000];
#endif

#ifdef RC_DLL
/* Must match `game::Host` in tools/revs-run/src/game.rs. */
#define RC_DLL_ABI 1u
typedef struct RcHost {
    uint32_t size; /* sizeof(RcHost), checked */
    void (*import_)(Cpu *c, uint32_t stub, uint32_t index);
    void (*miss)(Cpu *c, uint32_t target);
    int (*trace)(Cpu *c, uint32_t addr);
} RcHost;
static RcHost rc_host;

void rc_import(Cpu *c, uint32_t stub, uint32_t index) { rc_host.import_(c, stub, index); }
void rc_miss(Cpu *c, uint32_t target) { rc_host.miss(c, target); }
int rc_trace(Cpu *c, uint32_t addr) { return rc_host.trace(c, addr); }

/* Returns RC_DLL_ABI, or 0 if the runner's table has another layout. */
RC_API uint32_t rc_dll_init(const RcHost *h) {
    if (h->size != sizeof(RcHost)) return 0;
    rc_host = *h;
    return RC_DLL_ABI;
}

RC_API uint8_t *rc_cov_ptr(void) { return rc_cov; }
RC_API uint32_t rc_cpu_size(void) { return (uint32_t)sizeof(Cpu); }
#endif
