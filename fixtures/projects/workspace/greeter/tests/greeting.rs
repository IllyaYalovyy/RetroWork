#[test]
fn greeting_ends_with_an_exclamation_mark() {
    assert!(greeter::greeting("you").ends_with('!'));
}
