//! Submitted cache URLs: whatever is accepted is canonical, safe to list in a
//! `u|` line, and stable under normalisation.
#![no_main]

use gwc_core::cache_url::normalize;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(u) = normalize(raw) {
        assert!(u.url.starts_with("http://"));
        assert!((12..=255).contains(&u.url.len()), "{}", u.url);
        assert!(
            !u.url
                .chars()
                .any(|c| matches!(c, '?' | '#' | '@' | '|' | '\\' | '\r' | '\n')
                    || c.is_whitespace()),
            "{}",
            u.url
        );
        assert!(u.url.is_ascii());
        assert_eq!(normalize(&u.url).as_ref(), Ok(&u), "not stable");
    }
});
