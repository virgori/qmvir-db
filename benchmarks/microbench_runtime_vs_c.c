#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

typedef struct {
    const char *name;
    int64_t ops;
    double elapsed_ms;
    double ops_per_sec;
    uint64_t checksum;
} bench_result_t;

static volatile uint64_t g_sink = 0;
static void *(*malloc_fn)(size_t) = malloc;
static void (*free_fn)(void *) = free;

typedef struct {
    int64_t loop_iters;
    int64_t arith_iters;
    int fib_recursive_n;
    int64_t fib_iterative_repeats;
    int fib_iterative_n;
    int64_t simple_calls;
    int64_t many_arg_calls;
    int64_t string_repeats;
    int64_t array_size;
    int64_t hashmap_size;
    int64_t file_small_reads;
    int64_t file_medium_reads;
    int64_t alloc_iters;
    int64_t alloc_size;
} profile_t;

static profile_t profile_for(const char *profile) {
    if (strcmp(profile, "standard") == 0) {
        return (profile_t){
            6000000, 6000000, 33, 1200000, 90, 6000000, 3000000,
            1000000, 6000000, 900000, 1000, 120, 900000, 64,
        };
    }
    if (strcmp(profile, "heavy") == 0) {
        return (profile_t){
            12000000, 12000000, 35, 2400000, 90, 12000000, 6000000,
            2000000, 12000000, 1800000, 2000, 240, 1800000, 64,
        };
    }
    return (profile_t){
        2000000, 2000000, 30, 500000, 90, 2000000, 1000000,
        400000, 2000000, 300000, 400, 40, 300000, 64,
    };
}

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ((double)ts.tv_sec * 1000.0) + ((double)ts.tv_nsec / 1000000.0);
}

static uint64_t fib_recursive(int n) {
    if (n < 2) {
        return (uint64_t)n;
    }
    return fib_recursive(n - 1) + fib_recursive(n - 2);
}

static __attribute__((noinline)) uint64_t fib_iterative(int n) {
    uint64_t a = 0;
    uint64_t b = 1;
    for (int i = 0; i < n; i++) {
        uint64_t t = a + b;
        a = b;
        b = t;
    }
    return a;
}

static __attribute__((noinline)) uint64_t simple_fn(uint64_t v) {
    return v + 1;
}

static __attribute__((noinline)) uint64_t many_args_fn(
    uint64_t a,
    uint64_t b,
    uint64_t c,
    uint64_t d,
    uint64_t e,
    uint64_t f,
    uint64_t g,
    uint64_t h
) {
    return (a + b) ^ (c + d) ^ (e + f) ^ (g + h);
}

static uint64_t read_file_len_sum(const char *path, int64_t reads) {
    uint64_t sum = 0;
    for (int64_t i = 0; i < reads; i++) {
        FILE *f = fopen(path, "rb");
        if (!f) {
            return sum;
        }
        if (fseek(f, 0, SEEK_END) != 0) {
            fclose(f);
            continue;
        }
        long len = ftell(f);
        if (len < 0) {
            fclose(f);
            continue;
        }
        sum += (uint64_t)len;
        fclose(f);
    }
    return sum;
}

typedef struct {
    uint64_t key;
    uint64_t val;
    int used;
} hm_slot_t;

static uint64_t hm_hash(uint64_t x) {
    x ^= x >> 33;
    x *= 0xff51afd7ed558ccdULL;
    x ^= x >> 33;
    x *= 0xc4ceb9fe1a85ec53ULL;
    x ^= x >> 33;
    return x;
}

static uint64_t hashmap_bench(int64_t n) {
    int64_t cap = 1;
    while (cap < (n * 2)) {
        cap <<= 1;
    }

    hm_slot_t *tab = (hm_slot_t *)calloc((size_t)cap, sizeof(hm_slot_t));
    if (!tab) {
        return 0;
    }

    for (int64_t i = 0; i < n; i++) {
        uint64_t key = (uint64_t)i + 1;
        uint64_t h = hm_hash(key);
        int64_t idx = (int64_t)(h & (uint64_t)(cap - 1));
        while (tab[idx].used) {
            idx = (idx + 1) & (cap - 1);
        }
        tab[idx].used = 1;
        tab[idx].key = key;
        tab[idx].val = (uint64_t)i;
    }

    uint64_t sum = 0;
    for (int64_t i = 0; i < n; i++) {
        uint64_t key = (uint64_t)i + 1;
        uint64_t h = hm_hash(key);
        int64_t idx = (int64_t)(h & (uint64_t)(cap - 1));
        while (tab[idx].used) {
            if (tab[idx].key == key) {
                sum += tab[idx].val;
                break;
            }
            idx = (idx + 1) & (cap - 1);
        }
    }

    free(tab);
    return sum;
}

static bench_result_t mk_result(const char *name, int64_t ops, double t0_ms, uint64_t checksum) {
    double elapsed_ms = now_ms() - t0_ms;
    if (elapsed_ms <= 0.0) {
        elapsed_ms = 0.001;
    }
    double ops_per_sec = elapsed_ms > 0.0 ? ((double)ops * 1000.0) / elapsed_ms : 0.0;
    bench_result_t r = {name, ops, elapsed_ms, ops_per_sec, checksum};
    return r;
}

static void print_result(const bench_result_t *r) {
    printf("%s\t%lld\t%.6f\t%.3f\t%llu\n",
           r->name,
           (long long)r->ops,
           r->elapsed_ms,
           r->ops_per_sec,
           (unsigned long long)r->checksum);
}

int main(int argc, char **argv) {
    const char *profile = argc > 1 ? argv[1] : "quick";
    const char *small_path = argc > 2 ? argv[2] : "/tmp/qm_small.bin";
    const char *medium_path = argc > 3 ? argv[3] : "/tmp/qm_medium.bin";

    profile_t cfg = profile_for(profile);

    /* for_loop_large */
    double t0 = now_ms();
    uint64_t s = 0;
    for (int64_t i = 0; i < cfg.loop_iters; i++) {
        s += (uint64_t)i;
        g_sink ^= s;
    }
    bench_result_t r1 = mk_result("for_loop_large", cfg.loop_iters, t0, s);
    print_result(&r1);

    /* while_loop_large */
    t0 = now_ms();
    s = 0;
    int64_t i = 0;
    while (i < cfg.loop_iters) {
        s += (uint64_t)i;
        g_sink ^= s;
        i++;
    }
    bench_result_t r2 = mk_result("while_loop_large", cfg.loop_iters, t0, s);
    print_result(&r2);

    /* int_arithmetic_add_sub_mul_div */
    t0 = now_ms();
    uint64_t x = 7;
    uint64_t y = 3;
    uint64_t acc = 0;
    for (int64_t k = 1; k <= cfg.arith_iters; k++) {
        x = (x + (uint64_t)k) & 0xFFFFFFFFULL;
        y = (y * 3ULL + 1ULL) & 0xFFFFFFFFULL;
        acc += x + y;
        acc -= x - y;
        acc += x * y;
        acc += x / (y | 1ULL);
        g_sink ^= acc;
    }
    bench_result_t r3 = mk_result("int_arithmetic_add_sub_mul_div", cfg.arith_iters, t0, acc);
    print_result(&r3);

    /* fibonacci_recursive */
    t0 = now_ms();
    uint64_t fr = fib_recursive(cfg.fib_recursive_n);
    bench_result_t r4 = mk_result("fibonacci_recursive", 1, t0, fr);
    print_result(&r4);

    /* fibonacci_iterative */
    t0 = now_ms();
    uint64_t fi_sum = 0;
    for (int64_t rep = 0; rep < cfg.fib_iterative_repeats; rep++) {
        int n = cfg.fib_iterative_n + (int)(rep & 1);
        fi_sum += fib_iterative(n);
        g_sink ^= fi_sum;
    }
    bench_result_t r5 = mk_result("fibonacci_iterative", cfg.fib_iterative_repeats, t0, fi_sum);
    print_result(&r5);

    /* function_call_simple */
    t0 = now_ms();
    uint64_t cs = 0;
    for (int64_t n = 0; n < cfg.simple_calls; n++) {
        cs += simple_fn((uint64_t)n);
        g_sink ^= cs;
    }
    bench_result_t r6 = mk_result("function_call_simple", cfg.simple_calls, t0, cs);
    print_result(&r6);

    /* function_call_many_args */
    t0 = now_ms();
    uint64_t cm = 0;
    for (int64_t n = 0; n < cfg.many_arg_calls; n++) {
        cm += many_args_fn((uint64_t)n, (uint64_t)n + 1, (uint64_t)n + 2, (uint64_t)n + 3,
                           (uint64_t)n + 4, (uint64_t)n + 5, (uint64_t)n + 6, (uint64_t)n + 7);
        g_sink ^= cm;
    }
    bench_result_t r7 = mk_result("function_call_many_args", cfg.many_arg_calls, t0, cm);
    print_result(&r7);

    /* string_len_concat_basic */
    t0 = now_ms();
    uint64_t slen = 0;
    for (int64_t n = 0; n < cfg.string_repeats; n++) {
        char buf[64];
        int w = snprintf(buf, sizeof(buf), "qmvir%lldbenchmark", (long long)(n & 1023));
        if (w > 0) {
            slen += (uint64_t)w;
            g_sink ^= slen;
        }
    }
    bench_result_t r8 = mk_result("string_len_concat_basic", cfg.string_repeats, t0, slen);
    print_result(&r8);

    /* array_traversal */
    int64_t arr_n = cfg.array_size;
    uint8_t *arr = (uint8_t *)malloc((size_t)arr_n);
    if (!arr) {
        return 1;
    }
    for (int64_t n = 0; n < arr_n; n++) {
        arr[n] = (uint8_t)(n & 255);
    }
    t0 = now_ms();
    uint64_t asum = 0;
    for (int64_t n = 0; n < arr_n; n++) {
        asum += arr[n];
        g_sink ^= asum;
    }
    bench_result_t r9 = mk_result("array_traversal", cfg.array_size, t0, asum);
    print_result(&r9);
    free(arr);

    /* hashmap_insert_lookup */
    t0 = now_ms();
    uint64_t hsum = hashmap_bench(cfg.hashmap_size);
    bench_result_t r10 = mk_result("hashmap_insert_lookup", cfg.hashmap_size * 2, t0, hsum);
    print_result(&r10);

    /* file_read_small */
    t0 = now_ms();
    uint64_t fss = read_file_len_sum(small_path, cfg.file_small_reads);
    bench_result_t r11 = mk_result("file_read_small", cfg.file_small_reads, t0, fss);
    print_result(&r11);

    /* file_read_medium */
    t0 = now_ms();
    uint64_t fms = read_file_len_sum(medium_path, cfg.file_medium_reads);
    bench_result_t r12 = mk_result("file_read_medium", cfg.file_medium_reads, t0, fms);
    print_result(&r12);

    /* alloc_free_runtime */
    t0 = now_ms();
    uint64_t am = 0;
    for (int64_t n = 0; n < cfg.alloc_iters; n++) {
        uint8_t *p = (uint8_t *)malloc_fn((size_t)cfg.alloc_size);
        if (!p) {
            continue;
        }
        memset(p, 0, (size_t)cfg.alloc_size);
        p[0] = (uint8_t)(n & 255);
        am += p[0];
        g_sink ^= am;
        free_fn(p);
    }
    bench_result_t r13 = mk_result("alloc_free_runtime", cfg.alloc_iters, t0, am);
    print_result(&r13);

    return 0;
}
