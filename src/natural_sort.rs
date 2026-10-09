use std::cmp::Ordering;

#[derive(Debug)]
enum Part {
    Text(String),
    Number(u64),
}

fn split_natural(s: &str) -> Vec<Part> {
    let mut result = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut number = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    number.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            match number.parse::<u64>() {
                Ok(n) => result.push(Part::Number(n)),
                Err(_) => result.push(Part::Text(number)),
            }
        } else {
            let mut text = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    break;
                }
                text.push(c);
                chars.next();
            }
            result.push(Part::Text(text));
        }
    }

    result
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a_parts = split_natural(a);
    let b_parts = split_natural(b);

    for (x, y) in a_parts.iter().zip(b_parts.iter()) {
        let ordering = match (x, y) {
            (Part::Number(a), Part::Number(b)) => a.cmp(b),
            (Part::Text(a), Part::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
            (Part::Number(_), Part::Text(_)) => Ordering::Less,
            (Part::Text(_), Part::Number(_)) => Ordering::Greater,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    a_parts.len().cmp(&b_parts.len())
}
