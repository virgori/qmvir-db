#[cfg(test)]
mod tests {
    use rayon::prelude::*;
    use std::collections::HashMap;
    use std::env;
    use std::time::Instant;

    fn sequential_hash_join_count(build: &[u64], probe: &[u64]) -> usize {
        let mut ht: HashMap<u64, usize> = HashMap::new();
        for k in build {
            *ht.entry(*k).or_insert(0) += 1;
        }
        probe.iter().map(|k| ht.get(k).copied().unwrap_or(0)).sum()
    }

    fn parallel_hash_join_count(build: &[u64], probe: &[u64]) -> usize {
        let mut ht: HashMap<u64, usize> = HashMap::new();
        for k in build {
            *ht.entry(*k).or_insert(0) += 1;
        }
        probe
            .par_chunks(4096)
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|k| ht.get(k).copied().unwrap_or(0))
                    .sum::<usize>()
            })
            .sum()
    }

    fn gen_keys(n: usize, cardinality: u64, seed: u64) -> Vec<u64> {
        let mut x = seed.max(1);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            // tiny LCG for deterministic benchmark input
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            out.push((x >> 32) % cardinality.max(1));
        }
        out
    }

    fn parse_cases() -> Vec<usize> {
        let raw = env::var("HASH_JOIN_BENCH_CASES").unwrap_or_else(|_| "100000,300000".to_string());
        let mut out = Vec::new();
        for s in raw.split(',') {
            if let Ok(v) = s.trim().parse::<usize>() {
                if v > 0 {
                    out.push(v);
                }
            }
        }
        if out.is_empty() {
            vec![100000, 300000]
        } else {
            out
        }
    }

    #[test]
    fn join_ab_microbench() {
        let sizes = parse_cases();
        let cardinality = 8192u64;

        for n in sizes {
            let build = gen_keys(n, cardinality, 11);
            let probe = gen_keys(n, cardinality, 29);

            let t0 = Instant::now();
            let seq_out = sequential_hash_join_count(&build, &probe);
            let seq_ms = t0.elapsed().as_secs_f64() * 1000.0;

            let t1 = Instant::now();
            let par_out = parallel_hash_join_count(&build, &probe);
            let par_ms = t1.elapsed().as_secs_f64() * 1000.0;

            assert_eq!(seq_out, par_out);
            eprintln!(
                "join_ab_microbench n={} seq_ms={:.3} par_ms={:.3} speedup={:.2}x",
                n,
                seq_ms,
                par_ms,
                if par_ms > 0.0 { seq_ms / par_ms } else { 0.0 }
            );
        }
    }
}
