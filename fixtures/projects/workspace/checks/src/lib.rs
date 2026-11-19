pub fn answer() -> u32 {
    41
}

#[cfg(test)]
mod tests {
    #[test]
    fn answer_is_positive() {
        assert!(super::answer() > 0);
    }

    #[test]
    fn answer_is_forty_two() {
        assert_eq!(super::answer(), 42);
    }
}
