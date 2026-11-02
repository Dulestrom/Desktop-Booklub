//! QA-17: every colour lives in the token stylesheet.

use std::fs;
use std::path::Path;

fn hex_literal(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    (0..bytes.len()).find_map(|start| {
        if bytes[start] != b'#' {
            return None;
        }
        let digits = bytes[start + 1..].iter().take_while(|b| b.is_ascii_hexdigit()).count();
        let after = bytes.get(start + 1 + digits).copied().unwrap_or(b' ');
        (matches!(digits, 3 | 4 | 6 | 8) && !after.is_ascii_alphanumeric()).then(|| &line[start..start + 1 + digits])
    })
}

#[test]
fn no_colour_literal_outside_the_token_stylesheet() {
    let mut found = Vec::new();
    for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("src")).unwrap() {
        let path = entry.unwrap().path();
        for (number, line) in fs::read_to_string(&path).unwrap().lines().enumerate() {
            if let Some(literal) = hex_literal(line).filter(|_| !line.trim_start().starts_with("//")) {
                found.push(format!("{}:{} {literal}", path.display(), number + 1));
            }
        }
    }
    assert!(found.is_empty(), "colour literals outside style.css: {found:#?}");
}

#[test]
fn rules_use_variables_and_only_the_token_blocks_hold_values() {
    let css = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/style.css")).unwrap();
    let rules = &css[css.find("/* ---------- Type ramp").expect("rules section")..];
    let literals: Vec<_> = rules.lines().filter_map(hex_literal).collect();
    assert!(literals.is_empty(), "rules should reference tokens, found {literals:?}");
    assert!(css.contains("@media (prefers-color-scheme: dark)"), "REQ-42 needs the dark block");
}

fn luminance(rgb: [f64; 3]) -> f64 {
    let channel = |value: f64| {
        let v = value / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

fn hex_token(css: &str, name: &str) -> [f64; 3] {
    let start = css.find(&format!("{name}: #")).expect("token") + name.len() + 3;
    let hex = &css[start..start + 6];
    let part = |at: usize| f64::from(u8::from_str_radix(&hex[at..at + 2], 16).unwrap());
    [part(0), part(2), part(4)]
}

/// QA-11 on the worst artwork there is: a pure white image behind the Category name.
#[test]
fn category_text_passes_contrast_over_pure_white() {
    let css = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/style.css")).unwrap();

    // The name's top edge: a 122 px tile, less the 17 px margin, the 16 px count line, the 1 px
    // gap and the 22 px name line.
    let name_top_percent = (122.0 - 17.0 - 16.0 - 1.0 - 22.0) / 122.0 * 100.0;
    let rule = css.lines().find(|line| line.starts_with(".category-scrim")).expect("the scrim rule");
    let stop = rule.split("var(--tile-scrim) ").nth(1).and_then(|rest| rest.split('%').next()).expect("a stop for the 70% scrim");
    let stop: f64 = stop.parse().unwrap();
    assert!(stop <= name_top_percent, "the scrim reaches 70% at {stop}%, below the name's top at {name_top_percent:.0}%");
    assert!(css.contains("--tile-scrim: rgba(18, 14, 10, 0.7);"), "the check below assumes this value");

    let scrim = hex_token(&css, "--tile-scrim-solid");
    let over_white = [0, 1, 2].map(|i| 0.7 * scrim[i] + 0.3 * 255.0);
    let (ink, background) = (luminance(hex_token(&css, "--tile-ink")), luminance(over_white));
    let ratio = (ink.max(background) + 0.05) / (ink.min(background) + 0.05);
    assert!(ratio >= 4.5, "tile text measures {ratio:.2}:1 over white");
}
