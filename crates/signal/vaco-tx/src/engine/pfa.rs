//! Good–Thomas (prime-factor) decomposition.
//!
//! For `N = N₁·N₂` with `gcd(N₁, N₂) = 1`, the Ruritanian input map
//! `n = (N₂·n₁ + N₁·n₂) mod N` and the CRT output map
//! `k = (N₂·(N₂⁻¹ mod N₁)·k₁ + N₁·(N₁⁻¹ mod N₂)·k₂) mod N` turn the DFT into
//! `N₁` transforms of length `N₂` followed by `N₂` of length `N₁`, **with no
//! twiddle multiplies between them**. For an Opus length like `120 = 8·3·5`
//! that is a real saving, and it is why the rule sits ahead of Rader.
//!
//! The cost is two index permutations and a transpose. Both are `O(N)` against
//! an `O(N log N)` transform, and the permutation tables are built once.

use super::Ctx;
use super::Engine;
use crate::factor;
use crate::num::Arith;

#[derive(Debug, Clone)]
pub(crate) struct PrimeFactor<T: Arith> {
    n: usize,
    n1: usize,
    n2: usize,
    /// Row-major `[n₁][n₂]` grid slot → source index.
    in_map: Vec<u32>,
    /// Row-major `[n₂][n₁]` grid slot → destination index.
    out_map: Vec<u32>,
    sub1: Engine<T>,
    sub2: Engine<T>,
}

impl<T: Arith> PrimeFactor<T> {
    /// `None` unless `n1` is a proper coprime divisor of `n`.
    #[allow(
        clippy::integer_division,
        reason = "n1 divides n by the guard immediately above"
    )]
    pub(crate) fn new(n: usize, n1: usize, depth: u32) -> Option<Self> {
        if n1 <= 1 || n1 >= n || !n.is_multiple_of(n1) {
            return None;
        }
        let n2 = n / n1;
        if factor::gcd(n1, n2) != 1 {
            return None;
        }
        let inv2 = factor::mod_inverse(n2 % n1, n1)?;
        let inv1 = factor::mod_inverse(n1 % n2, n2)?;

        let mut in_map = vec![0u32; n];
        for i in 0..n1 {
            for j in 0..n2 {
                let idx = (n2 as u128 * i as u128 + n1 as u128 * j as u128) % n as u128;
                if let Some(slot) = in_map.get_mut(i * n2 + j) {
                    *slot = idx as u32;
                }
            }
        }
        let mut out_map = vec![0u32; n];
        let c1 = (n2 as u128 * inv2 as u128) % n as u128;
        let c2 = (n1 as u128 * inv1 as u128) % n as u128;
        for j in 0..n2 {
            for i in 0..n1 {
                let idx = (c1 * i as u128 + c2 * j as u128) % n as u128;
                if let Some(slot) = out_map.get_mut(j * n1 + i) {
                    *slot = idx as u32;
                }
            }
        }

        Some(Self {
            n,
            n1,
            n2,
            in_map,
            out_map,
            sub1: Engine::build(n1, depth + 1),
            sub2: Engine::build(n2, depth + 1),
        })
    }

    pub(crate) fn scratch_len(&self) -> usize {
        4 * self.n + self.sub1.scratch_len().max(self.sub2.scratch_len())
    }

    pub(crate) fn describe(&self) -> crate::Decomposition {
        crate::Decomposition::PrimeFactor {
            factors: [self.n1, self.n2],
            sub: vec![self.sub1.describe(), self.sub2.describe()],
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "in_map and out_map hold residues mod n; grid indices are i·n₂+j < n and j·n₁+i < n; all buffers are length-checked on entry"
    )]
    pub(crate) fn exec(&self, re: &mut [T], im: &mut [T], scratch: &mut [T], ctx: Ctx) {
        let (n, n1, n2) = (self.n, self.n1, self.n2);
        if re.len() < n || im.len() < n || scratch.len() < self.scratch_len() {
            debug_assert!(false, "buffer too small for Good-Thomas({n})");
            return;
        }
        let (a_re, rest) = scratch.split_at_mut(n);
        let (a_im, rest) = rest.split_at_mut(n);
        let (b_re, rest) = rest.split_at_mut(n);
        let (b_im, sub) = rest.split_at_mut(n);

        for g in 0..n {
            let src = self.in_map[g] as usize;
            a_re[g] = re[src];
            a_im[g] = im[src];
        }
        for i in 0..n1 {
            let lo = i * n2;
            self.sub2
                .exec(&mut a_re[lo..lo + n2], &mut a_im[lo..lo + n2], sub, ctx);
        }
        for i in 0..n1 {
            for j in 0..n2 {
                b_re[j * n1 + i] = a_re[i * n2 + j];
                b_im[j * n1 + i] = a_im[i * n2 + j];
            }
        }
        for j in 0..n2 {
            let lo = j * n1;
            self.sub1
                .exec(&mut b_re[lo..lo + n1], &mut b_im[lo..lo + n1], sub, ctx);
        }
        for g in 0..n {
            let dst = self.out_map[g] as usize;
            re[dst] = b_re[g];
            im[dst] = b_im[g];
        }
    }
}
