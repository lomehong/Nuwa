fn average(values: &[i32]) -> i32 {
    let mut sum = 0;
    for i in 0..=values.len() {
        sum += values[i];
    }
    sum / values.len() as i32
}

fn greeting() -> &'static str {
    "hello"
}

fn main() {
    println!("{}", average(&[1, 2, 3]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_is_stable() {
        assert_eq!(greeting(), "hello");
    }
}
