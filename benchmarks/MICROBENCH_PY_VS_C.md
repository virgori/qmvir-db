# Microbenchmark: Vir vs C vs Python

Profile: `quick`

| Benchmark | Python ms | C ms | Vir ms | Python ops/s | C ops/s | Vir ops/s | C/Python | Vir/Python | C/Vir |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| for_loop_large | 65.472 | 0.886 | 8.796 | 30547487.03 | 2257336343.27 | 9094613.08 | 73.90x | 0.30x | 248.21x |
| while_loop_large | 91.007 | 1.039 | 8.160 | 21976411.90 | 1924927820.62 | 9804422.60 | 87.59x | 0.45x | 196.33x |
| int_arithmetic_add_sub_mul_div | 800.713 | 4.354 | 19.885 | 2497773.47 | 459347726.00 | 4023208.89 | 183.90x | 1.61x | 114.17x |
| fibonacci_recursive | 121.567 | 3.175 | 3.360 | 8.23 | 314.96 | 297.60 | 38.29x | 36.18x | 1.06x |
| fibonacci_iterative | 1229.790 | 21.380 | 176.962 | 406573.59 | 23386342.38 | 169528.08 | 57.52x | 0.42x | 137.95x |
| function_call_simple | 133.719 | 2.500 | 22.192 | 14956723.65 | 800000000.00 | 3604895.84 | 53.49x | 0.24x | 221.92x |
| function_call_many_args | 247.062 | 1.323 | 29.391 | 4047574.51 | 755857897.20 | 2041421.05 | 186.74x | 0.50x | 370.26x |
| string_len_concat_basic | 57.975 | 29.591 | 10.184 | 6899495.91 | 13517623.60 | 5891763.01 | 1.96x | 0.85x | 2.29x |
| array_traversal | 42.730 | 0.976 | 13.571 | 46805066.28 | 2049180328.31 | 5894959.49 | 43.78x | 0.13x | 347.62x |
| hashmap_insert_lookup | 141.413 | 8.507 | SKIP | 4242888.90 | 70530151.65 | SKIP | 16.62x | 0.00x | 0.00x |
| file_read_small | 11.351 | 7.231 | 3.482 | 35238.54 | 55317.38 | 38196.90 | 1.57x | 1.08x | 1.45x |
| file_read_medium | 1.892 | 0.742 | 3.053 | 21144.91 | 53908.36 | 4913.74 | 2.55x | 0.23x | 10.97x |
| alloc_free_runtime | 37.706 | 0.157 | 11.907 | 7956337.32 | 1910828052.32 | 5039193.59 | 240.16x | 0.63x | 379.19x |

Notes:
- Vir results are measured by wall-clock timing around `vir run <case>.vir`.
- `hashmap_insert_lookup` is currently `SKIP` on Vir bootstrap runtime in this harness.
- Vir config is downscaled from profile for practical run time: loop=80000, arith=80000, fib_rec_n=28.
