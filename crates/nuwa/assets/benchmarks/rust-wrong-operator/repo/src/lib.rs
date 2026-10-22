pub fn total(values: &[i32]) -> i32 {
    let mut sum = 0;
    for value in values {
        sum -= *value;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_values() {
        assert_eq!(total(&[1, 2, 3]), 6);
    }
}
