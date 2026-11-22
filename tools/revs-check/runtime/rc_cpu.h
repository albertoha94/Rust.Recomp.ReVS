/* Runtime for recompiled ARM code (game-independent; part of the repo).
 *
 * Guest memory is one flat 4 GiB block: guest address a is host byte m + a,
 * so pointers stay 32-bit and data layout is byte-identical to the device.
 * Flags are kept as separate 0/1 words. The generated C (which IS derived from
 * the game, and so lives only in the git-ignored build/ folder) includes this.
 */
#ifndef RC_CPU_H
#define RC_CPU_H

#include <stdint.h>
#include <string.h>

typedef struct Cpu {
    uint32_t r[16];
    uint32_t n, z, c, v;
    uint8_t *m;        /* guest memory base */
    int32_t budget;    /* backward branches left before giving up (loop guard) */
    uint32_t halt;     /* nonzero: stop and return up the C call stack */
    uint32_t halt_addr;
    /* VFP: s0-s63 as raw words; d<n> is ext[2n] (low) + ext[2n+1] (high). */
    uint32_t ext[64];
    uint32_t fpscr;    /* NZCV in bits 31-28 (from vcmp); control bits stored only */
} Cpu;

typedef void (*RcFn)(Cpu *);

/* Halt reasons */
#define RC_HALT_TIMEOUT 1u
#define RC_HALT_TRAP 2u     /* call to a function that is not translated */
#define RC_HALT_IMPORT 3u   /* call to an OS function (no native layer yet) */

static inline uint32_t rc_rd32(Cpu *c, uint32_t a) { uint32_t v; memcpy(&v, c->m + a, 4); return v; }
static inline uint32_t rc_rd16(Cpu *c, uint32_t a) { uint16_t v; memcpy(&v, c->m + a, 2); return v; }
static inline uint32_t rc_rd8(Cpu *c, uint32_t a) { return c->m[a]; }
static inline void rc_wr32(Cpu *c, uint32_t a, uint32_t v) { memcpy(c->m + a, &v, 4); }
static inline void rc_wr16(Cpu *c, uint32_t a, uint32_t v) { uint16_t h = (uint16_t)v; memcpy(c->m + a, &h, 2); }
static inline void rc_wr8(Cpu *c, uint32_t a, uint32_t v) { c->m[a] = (uint8_t)v; }

static inline uint32_t rc_ror(uint32_t x, uint32_t n) {
    n &= 31;
    return n ? (x >> n) | (x << (32 - n)) : x;
}

/* Shifts by a register amount (bottom byte), with carry out. */
static inline uint32_t rc_lsl_c(uint32_t x, uint32_t s, uint32_t *c) {
    s &= 0xFF;
    if (s == 0) return x;
    if (s < 32) { *c = (x >> (32 - s)) & 1; return x << s; }
    *c = (s == 32) ? (x & 1) : 0;
    return 0;
}
static inline uint32_t rc_lsr_c(uint32_t x, uint32_t s, uint32_t *c) {
    s &= 0xFF;
    if (s == 0) return x;
    if (s < 32) { *c = (x >> (s - 1)) & 1; return x >> s; }
    *c = (s == 32) ? (x >> 31) : 0;
    return 0;
}
static inline uint32_t rc_asr_c(uint32_t x, uint32_t s, uint32_t *c) {
    s &= 0xFF;
    if (s == 0) return x;
    if (s < 32) { *c = (uint32_t)((int32_t)x >> (s - 1)) & 1; return (uint32_t)((int32_t)x >> s); }
    *c = x >> 31;
    return (uint32_t)((int32_t)x >> 31);
}
static inline uint32_t rc_ror_c(uint32_t x, uint32_t s, uint32_t *c) {
    s &= 0xFF;
    if (s == 0) return x;
    if ((s & 31) == 0) { *c = x >> 31; return x; }
    *c = (x >> ((s & 31) - 1)) & 1;
    return rc_ror(x, s);
}

/* a + b + cin; with s, sets NZCV. Subtraction is a + ~b + 1. */
static inline uint32_t rc_add(Cpu *c, uint32_t a, uint32_t b, uint32_t cin, int s) {
    uint64_t r = (uint64_t)a + b + cin;
    uint32_t res = (uint32_t)r;
    if (s) {
        c->n = res >> 31;
        c->z = res == 0;
        c->c = (uint32_t)(r >> 32);
        c->v = ((~(a ^ b)) & (a ^ res)) >> 31;
    }
    return res;
}

static inline void rc_nz(Cpu *c, uint32_t res) { c->n = res >> 31; c->z = res == 0; }

static inline uint32_t rc_clz(uint32_t x) {
    uint32_t n = 0;
    if (!x) return 32;
    while (!(x & 0x80000000u)) { x <<= 1; n++; }
    return n;
}

static inline uint32_t rc_rev(uint32_t x) {
    return (x >> 24) | ((x >> 8) & 0xFF00u) | ((x << 8) & 0xFF0000u) | (x << 24);
}

/* ---- VFP ----
 * Matches the ARM rules with FPSCR = 0 (what touchHLE runs the game with):
 * round to nearest, denormals kept, NaNs propagated (DN = 0). Values are held
 * as raw bits so NaN payloads survive; host float math does the rounding (the
 * host's IEEE single/double give the same correctly rounded results).
 * NaN rule (FPProcessNaNs): first signalling NaN (quieted), else first quiet
 * NaN, in operand order. An invalid operation gives the default NaN. */
static inline uint64_t rc_getd(Cpu *c, int n) { return ((uint64_t)c->ext[2 * n + 1] << 32) | c->ext[2 * n]; }
static inline void rc_setd(Cpu *c, int n, uint64_t v) { c->ext[2 * n] = (uint32_t)v; c->ext[2 * n + 1] = (uint32_t)(v >> 32); }

#define RC_FP_TYPE(S, U, F, QBIT, DNAN, ABS, INF)                                                                      \
    static inline F rc_bf_##S(U u) { F f; memcpy(&f, &u, sizeof f); return f; }                                   \
    static inline U rc_fb_##S(F f) { U u; memcpy(&u, &f, sizeof u); return u; }                                   \
    static inline int rc_isnan_##S(U a) { return (a & ABS) > INF; }                                             \
    static inline U rc_nan2_##S(U a, U b) {                                                                       \
        if (rc_isnan_##S(a) && !(a & QBIT)) return a | QBIT;                                                       \
        if (rc_isnan_##S(b) && !(b & QBIT)) return b | QBIT;                                                       \
        if (rc_isnan_##S(a)) return a;                                                                             \
        if (rc_isnan_##S(b)) return b;                                                                             \
        return 0;                                                                                                  \
    }                                                                                                              \
    static inline U rc_res_##S(F r) { U u = rc_fb_##S(r); return rc_isnan_##S(u) ? DNAN : u; }                    \
    static inline U rc_fadd_##S(U a, U b) { U n = rc_nan2_##S(a, b); return n ? n : rc_res_##S(rc_bf_##S(a) + rc_bf_##S(b)); } \
    static inline U rc_fsub_##S(U a, U b) { U n = rc_nan2_##S(a, b); return n ? n : rc_res_##S(rc_bf_##S(a) - rc_bf_##S(b)); } \
    static inline U rc_fmul_##S(U a, U b) { U n = rc_nan2_##S(a, b); return n ? n : rc_res_##S(rc_bf_##S(a) * rc_bf_##S(b)); } \
    static inline U rc_fdiv_##S(U a, U b) { U n = rc_nan2_##S(a, b); return n ? n : rc_res_##S(rc_bf_##S(a) / rc_bf_##S(b)); } \
    static inline U rc_fsqrt_##S(U a) {                                                                           \
        if (rc_isnan_##S(a)) return a | QBIT;                                                                      \
        if ((a & ~ABS) && (a & ABS)) return DNAN; /* negative, not -0 */                                           \
        return rc_fb_##S(sqrt##S##_(rc_bf_##S(a)));                                                                 \
    }                                                                                                              \
    /* vcmp: NZCV in bits 31-28. equal 0110, less 1000, greater 0010, unordered 0011 */                            \
    static inline uint32_t rc_fcmp_##S(U a, U b) {                                                                 \
        if (rc_isnan_##S(a) || rc_isnan_##S(b)) return 0x30000000u;                                                \
        F x = rc_bf_##S(a), y = rc_bf_##S(b);                                                                      \
        return x == y ? 0x60000000u : x < y ? 0x80000000u : 0x20000000u;                                           \
    }                                                                                                              \
    /* float -> int32 / uint32: round toward zero (rz) or to nearest, saturate, NaN -> 0 */                       \
    static inline uint32_t rc_ftosi_##S(U a, int rz) {                                                            \
        if (rc_isnan_##S(a)) return 0;                                                                             \
        double v = (double)rc_bf_##S(a);                                                                           \
        v = rz ? trunc(v) : nearbyint(v);                                                                          \
        if (v >= 2147483647.0) return 0x7FFFFFFFu;                                                                 \
        if (v <= -2147483648.0) return 0x80000000u;                                                                \
        return (uint32_t)(int32_t)v;                                                                               \
    }                                                                                                              \
    static inline uint32_t rc_ftoui_##S(U a, int rz) {                                                            \
        if (rc_isnan_##S(a)) return 0;                                                                             \
        double v = (double)rc_bf_##S(a);                                                                           \
        v = rz ? trunc(v) : nearbyint(v);                                                                          \
        if (v >= 4294967295.0) return 0xFFFFFFFFu;                                                                 \
        if (v <= 0.0) return 0;                                                                                    \
        return (uint32_t)v;                                                                                        \
    }

#include <math.h>
#define sqrts_ sqrtf
#define sqrtd_ sqrt
RC_FP_TYPE(s, uint32_t, float, 0x00400000u, 0x7FC00000u, 0x7FFFFFFFu, 0x7F800000u)
RC_FP_TYPE(d, uint64_t, double, 0x0008000000000000ull, 0x7FF8000000000000ull, 0x7FFFFFFFFFFFFFFFull, 0x7FF0000000000000ull)
#undef RC_FP_TYPE

static inline uint32_t rc_fneg_s(uint32_t a) { return a ^ 0x80000000u; }
static inline uint64_t rc_fneg_d(uint64_t a) { return a ^ 0x8000000000000000ull; }
static inline uint32_t rc_fabs_s(uint32_t a) { return a & 0x7FFFFFFFu; }
static inline uint64_t rc_fabs_d(uint64_t a) { return a & 0x7FFFFFFFFFFFFFFFull; }
/* int -> float (exact host conversion, round to nearest) */
static inline uint32_t rc_sitof_s(uint32_t a) { return rc_fb_s((float)(int32_t)a); }
static inline uint32_t rc_uitof_s(uint32_t a) { return rc_fb_s((float)a); }
static inline uint64_t rc_sitof_d(uint32_t a) { return rc_fb_d((double)(int32_t)a); }
static inline uint64_t rc_uitof_d(uint32_t a) { return rc_fb_d((double)a); }
/* single <-> double: NaNs are quieted and keep sign and top payload bits */
static inline uint64_t rc_stod(uint32_t a) {
    if (rc_isnan_s(a)) return ((uint64_t)(a >> 31) << 63) | 0x7FF8000000000000ull | ((uint64_t)(a & 0x003FFFFFu) << 29);
    return rc_fb_d((double)rc_bf_s(a));
}
static inline uint32_t rc_dtos(uint64_t a) {
    if (rc_isnan_d(a)) return ((uint32_t)(a >> 63) << 31) | 0x7FC00000u | (uint32_t)((a >> 29) & 0x003FFFFFu);
    return rc_fb_s((float)rc_bf_d(a));
}

/* ---- NEON (single-precision lanes) ----
 * NEON float math ignores FPSCR and always uses the "standard FPSCR": denormal
 * inputs and results are flushed to (signed) zero, and any NaN result is the
 * default NaN. */
static inline uint32_t rc_ftz_s(uint32_t a) { return (a & 0x7F800000u) == 0 ? (a & 0x80000000u) : a; }
static inline uint32_t rc_nres_s(float r) {
    uint32_t u = rc_fb_s(r);
    return rc_isnan_s(u) ? 0x7FC00000u : rc_ftz_s(u);
}
#define RC_NEON2(NAME, OP)                                                    \
    static inline uint32_t rc_n##NAME##_s(uint32_t a, uint32_t b) {           \
        a = rc_ftz_s(a);                                                      \
        b = rc_ftz_s(b);                                                      \
        if (rc_isnan_s(a) || rc_isnan_s(b)) return 0x7FC00000u;               \
        return rc_nres_s(rc_bf_s(a) OP rc_bf_s(b));                           \
    }
RC_NEON2(add, +)
RC_NEON2(sub, -)
RC_NEON2(mul, *)
#undef RC_NEON2

/* Provided by the generated table / host. */
void rc_call(Cpu *c, uint32_t target);           /* indirect call through the function table */
void rc_trap(Cpu *c, uint32_t addr, uint32_t why); /* stop: untranslated code or OS call */

/* Provided by the host (checker: stop; native runner: the OS function / a
 * call to a non-game address such as a host thunk). */
void rc_import(Cpu *c, uint32_t stub, uint32_t index);
void rc_miss(Cpu *c, uint32_t target);

#ifdef RC_NATIVE
#define RC_LOOP_GUARD() do { } while (0)
/* Function coverage for --coverage: one byte per game halfword below 0x400000
 * (libraries live above it and are not counted). Bit 0: the function ran.
 * Bit 1: rc_trace runs at the function's entry (REVS_TRACE_FN logging, the
 * runner's hooks). A non-zero return skips the function: it returns at once,
 * before it has touched anything (every translated function returns void). */
extern uint8_t rc_cov[0x200000];
int rc_trace(Cpu *c, uint32_t addr);
#define RC_ENTER(a) do { if ((a) < 0x400000u) { uint8_t *cov_ = &rc_cov[(a) >> 1]; *cov_ |= 1; if ((*cov_ & 2) && rc_trace(c, (a))) return; } } while (0)
#else
#define RC_ENTER(a) do { } while (0)
#define RC_LOOP_GUARD() do { if (--c->budget <= 0) { c->halt = RC_HALT_TIMEOUT; return; } } while (0)
#endif
#define RC_AFTER_CALL() do { if (c->halt) return; } while (0)

#endif
