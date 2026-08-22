//! Integer factorisation and the modular arithmetic the decomposition needs.
//!
//! All of it runs at plan time, never per transform, so clarity beats speed —
//! trial division is entirely adequate for lengths up to [`MAX_LEN`].
//!
//! The one property that matters: every function here is **total** over the
//! lengths `Plan::new` accepts. [`primitive_root`] and [`mod_inverse`] return
//! `Option` rather than asserting, so a decomposition rule that cannot apply
//! declines instead of panicking, and the planner falls through to Bluestein.

/// The radices [`crate::engine::stockham`] has kernels for, in the order the
/// planner emits them: largest first, so the sub-transform count `s` reaches the
/// SIMD lane width in as few stages as possible (see `docs/signal/vaco-tx.md`,
/// "The early stages").
pub(crate) const KERNEL_RADICES: [usize; 6] = [8, 7, 5, 4, 3, 2];

/// The primes the mixed-radix path can consume directly.
pub(crate) const KERNEL_PRIMES: [usize; 4] = [2, 3, 5, 7];

/// The largest transform length `Plan::new` will accept.
///
/// Not a limitation of the algorithms — a bound on how much a caller can make us
/// allocate from a length that, in a codec, came out of a bitstream. `2^24`
/// complex `f64` samples is 256 MiB per buffer; anything larger is a bug or an
/// attack, and D6 wants it rejected rather than attempted.
pub(crate) const MAX_LEN: usize = 1 << 24;

#[must_use]
pub(crate) fn is_prime(n: usize) -> bool {
    if n < 2 {
        return false;
    }
    if n.is_multiple_of(2) {
        return n == 2;
    }
    let mut d = 3usize;
    while d.saturating_mul(d) <= n {
        if n.is_multiple_of(d) {
            return false;
        }
        d += 2;
    }
    true
}

/// Prime-power factorisation, ascending by prime.
#[must_use]
pub(crate) fn factorise(mut n: usize) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let mut d = 2usize;
    while d.saturating_mul(d) <= n {
        if n.is_multiple_of(d) {
            let mut e = 0;
            while n.is_multiple_of(d) {
                n /= d;
                e += 1;
            }
            out.push((d, e));
        }
        d += if d == 2 { 1 } else { 2 };
    }
    if n > 1 {
        out.push((n, 1));
    }
    out
}

/// `base^exp mod m`, with `u128` intermediates so no product can wrap.
#[must_use]
pub(crate) fn pow_mod(base: usize, exp: usize, m: usize) -> usize {
    if m <= 1 {
        return 0;
    }
    let mut result: u128 = 1;
    let mut b = (base % m) as u128;
    let mut e = exp;
    let mm = m as u128;
    while e > 0 {
        if e & 1 == 1 {
            result = result * b % mm;
        }
        b = b * b % mm;
        e >>= 1;
    }
    result as usize
}

/// Modular inverse of `a` mod `m` by the extended Euclidean algorithm.
///
/// `None` when `gcd(a, m) != 1`.
#[must_use]
pub(crate) fn mod_inverse(a: usize, m: usize) -> Option<usize> {
    if m <= 1 {
        return None;
    }
    let (mut old_r, mut r) = (a as i128 % m as i128, m as i128);
    let (mut old_s, mut s) = (1i128, 0i128);
    while r != 0 {
        let q = old_r.div_euclid(r);
        (old_r, r) = (r, old_r - q * r);
        (old_s, s) = (s, old_s - q * s);
    }
    if old_r != 1 {
        return None;
    }
    Some(old_s.rem_euclid(m as i128) as usize)
}

#[must_use]
pub(crate) fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The smallest primitive root of an odd prime `p`.
///
/// `None` when `p` is not an odd prime, which is what makes Rader's rule
/// decline rather than assert.
#[must_use]
pub(crate) fn primitive_root(p: usize) -> Option<usize> {
    if p < 3 || !is_prime(p) {
        return None;
    }
    let phi = p - 1;
    let qs: Vec<usize> = factorise(phi).into_iter().map(|(q, _)| q).collect();
    #[allow(
        clippy::integer_division,
        reason = "q divides phi by construction, so phi/q is exact"
    )]
    {
        (2..p).find(|&g| qs.iter().all(|&q| pow_mod(g, phi / q, p) != 1))
    }
}

/// Split `n` into the stage radices [`crate::engine::stockham`] executes.
///
/// `None` when `n` has a prime factor outside [`KERNEL_PRIMES`]. Emitted
/// largest-radix-first: for `n = 1024` this is `[8, 8, 8, 2]`, so `s` runs
/// `1, 8, 64, 512` and only the first stage is below any plausible vector width.
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "`counts` is a 3-element array indexed only by literals and by the 3-element enumerate above"
)]
#[allow(
    clippy::integer_division,
    reason = "`e2 / 3` counts whole radix-8 stages; the remainder is used on the next line"
)]
pub(crate) fn smooth_radices(n: usize) -> Option<Vec<usize>> {
    if n < 2 {
        return None;
    }
    let mut rest = n;
    let mut e2 = 0u32;
    while rest.is_multiple_of(2) {
        rest /= 2;
        e2 += 1;
    }
    let mut counts = [0u32; 3]; // 3, 5, 7
    for (i, p) in [3usize, 5, 7].into_iter().enumerate() {
        while rest.is_multiple_of(p) {
            rest /= p;
            counts[i] += 1;
        }
    }
    if rest != 1 {
        return None;
    }

    let mut radices = Vec::new();
    let mut a = e2;
    radices.extend(core::iter::repeat_n(8usize, (a / 3) as usize));
    a %= 3;
    radices.extend(core::iter::repeat_n(7usize, counts[2] as usize));
    radices.extend(core::iter::repeat_n(5usize, counts[1] as usize));
    if a == 2 {
        radices.push(4);
    }
    radices.extend(core::iter::repeat_n(3usize, counts[0] as usize));
    if a == 1 {
        radices.push(2);
    }
    Some(radices)
}

/// The largest divisor of `n` built only from [`KERNEL_PRIMES`].
#[must_use]
pub(crate) fn smooth_part(n: usize) -> usize {
    let mut acc = 1usize;
    let mut rest = n;
    for p in KERNEL_PRIMES {
        while rest.is_multiple_of(p) {
            rest /= p;
            acc *= p;
        }
    }
    acc
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::integer_division)]
mod tests {
    use super::*;

    #[test]
    fn factorisation_round_trips() {
        for n in 1usize..2000 {
            let prod: usize = factorise(n).iter().map(|&(p, e)| p.pow(e)).product();
            assert_eq!(prod.max(1), n.max(1), "n={n}");
        }
    }

    #[test]
    fn primality_agrees_with_a_sieve() {
        let n = 5000usize;
        let mut sieve = vec![true; n];
        sieve[0] = false;
        sieve[1] = false;
        for i in 2..n {
            if !sieve[i] {
                continue;
            }
            let mut j = i * i;
            while j < n {
                sieve[j] = false;
                j += i;
            }
        }
        for (i, &want) in sieve.iter().enumerate() {
            assert_eq!(is_prime(i), want, "n={i}");
        }
    }

    #[test]
    fn primitive_roots_generate_the_whole_group() {
        for p in [3usize, 5, 7, 11, 13, 17, 19, 23, 97, 101, 1021] {
            let g = primitive_root(p).unwrap_or(0);
            assert!(g >= 2, "no primitive root for {p}");
            let mut seen = vec![false; p];
            let mut x = 1usize;
            for _ in 0..p - 1 {
                assert!(!seen[x], "g={g} repeats before order p-1 for p={p}");
                seen[x] = true;
                x = x * g % p;
            }
            assert_eq!(x, 1);
        }
    }

    #[test]
    fn inverses_are_inverses() {
        for m in [7usize, 12, 97, 1024] {
            for a in 1..m {
                match mod_inverse(a, m) {
                    Some(inv) => assert_eq!(a * inv % m, 1, "a={a} m={m}"),
                    None => assert!(gcd(a, m) > 1, "declined an invertible a={a} m={m}"),
                }
            }
        }
    }

    #[test]
    fn smooth_radices_multiply_back() {
        for n in [2usize, 4, 8, 16, 64, 120, 240, 480, 960, 1024, 2048, 4096, 5040] {
            let r = smooth_radices(n).unwrap_or_default();
            assert!(!r.is_empty(), "n={n} should be smooth");
            assert_eq!(r.iter().product::<usize>(), n, "n={n} radices {r:?}");
            assert!(r.iter().all(|x| KERNEL_RADICES.contains(x)));
        }
        assert!(smooth_radices(11).is_none());
        assert!(smooth_radices(121).is_none());
    }

    #[test]
    fn largest_radix_comes_first() {
        // 1024 = 8·8·8·2: s runs 1, 8, 64, 512.
        assert_eq!(smooth_radices(1024).unwrap_or_default(), vec![8, 8, 8, 2]);
    }
}
