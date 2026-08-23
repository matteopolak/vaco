fn main() {
    println!("enabled={}", vaco_registry::ENCUMBERED_ENABLED.len());
    println!("all={}", vaco_registry::ENCUMBERED_ALL.len());
    for name in vaco_registry::ENCUMBERED_ENABLED {
        println!("row={name}");
    }
}
