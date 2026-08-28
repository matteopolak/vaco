use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind};

#[test]
fn full_imdct_matches_reference_definition() -> Result<(), String> {
    let half = 4usize;
    let full = half * 2;
    let coeffs: Vec<f32> = vec![1.0, -0.5, 0.25, 2.0];
    let coeffs64: Vec<f64> = coeffs.iter().map(|&v| f64::from(v)).collect();
    let expected = vaco_tx::reference::imdct(&coeffs64);

    let plan = Plan::<f32>::new(TxKind::Mdct, Direction::Inverse, full, 1.0, TxFlags::FULL_IMDCT)
        .map_err(|e| format!("Plan::new failed: {e:?}"))?;
    let mut tx = Tx::new(plan);
    let mut out = vec![0.0f32; full];
    tx.execute(&mut out, &coeffs);

    eprintln!("expected = {expected:?}");
    eprintln!("actual   = {out:?}");
    for (a, b) in out.iter().zip(expected.iter()) {
        if (f64::from(*a) - b).abs() >= 1e-3 {
            return Err(format!("a={a} b={b}"));
        }
    }
    Ok(())
}
