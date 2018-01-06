pub fn encode(mut n: u64) -> String {
    let cap = (36 as f64).log(n as f64).ceil() as usize;
    let mut buf = Vec::with_capacity(cap);
    while n >= 36 {
        buf.push(ALPHABET[(n % 36) as usize]);
        n /= 36
    }
    buf.push(ALPHABET[n as usize]);
    buf.into_iter().rev().collect()
}

// PRIVATE

static ALPHABET: &'static [char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];