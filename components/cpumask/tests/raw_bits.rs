use ax_cpumask::CpuMask;

#[test]
fn raw_bits_at_word_width() {
    let mask = CpuMask::<{ usize::BITS as usize }>::from_raw_bits(usize::MAX);
    assert_eq!(mask.len(), usize::BITS as usize);
    assert!(mask.get(usize::BITS as usize - 1));
}

#[test]
fn raw_bits_into_wider_mask_are_zero_extended() {
    let mask = CpuMask::<128>::from_raw_bits(usize::MAX);
    assert_eq!(mask.len(), usize::BITS as usize);
    assert!(!mask.get(usize::BITS as usize));
}

#[test]
#[should_panic]
fn raw_bits_reject_bits_outside_narrow_mask() {
    CpuMask::<4>::from_raw_bits(0x10);
}
