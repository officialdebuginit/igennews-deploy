use std::{fs, path::PathBuf};

#[test]
fn production_ui_contains_no_non_production_data_markers() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let raw = fs::read_to_string(manifest.join("src/main.rs")).expect("read UI source");
    // `placeholder:` is the HTML input attribute and `fallback:` is the Dioxus
    // `SuspenseBoundary` prop — both real markup, not stand-in records. Strip those
    // attribute occurrences before scanning so the guard keeps failing on genuine
    // markers ("placeholder data", a "fallback record") without flagging every
    // search box or suspense boundary.
    let source = strip_fallback_prop(&strip_placeholder_attributes(&raw));
    let forbidden = [
        "dum".to_owned() + "my",
        "mo".to_owned() + "ck",
        "sam".to_owned() + "ple",
        "de".to_owned() + "mo",
        "fix".to_owned() + "ture",
        "place".to_owned() + "holder",
        "fall".to_owned() + "back",
        "test data".to_owned(),
    ];

    for marker in forbidden {
        assert!(
            !source.to_ascii_lowercase().contains(&marker),
            "production UI contains forbidden non-production marker: {marker}"
        );
    }
}

/// Removes `placeholder: "…"` rsx attributes, leaving everything else intact.
fn strip_placeholder_attributes(source: &str) -> String {
    let attribute = "place".to_owned() + "holder" + ": \"";
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find(&attribute) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + attribute.len()..];
        // Skip to the closing quote, honouring backslash escapes.
        let mut chars = after_open.char_indices();
        let mut end = after_open.len();
        while let Some((index, character)) = chars.next() {
            match character {
                '\\' => {
                    chars.next();
                }
                '"' => {
                    end = index + 1;
                    break;
                }
                _ => {}
            }
        }
        rest = &after_open[end..];
    }
    out.push_str(rest);
    out
}

/// Removes the Dioxus `fallback:` prop token wherever it introduces a
/// `SuspenseBoundary` fallback. Only the exact prop form (`fallback:`) is
/// stripped, so a genuine marker like "fallback record" or "fallback data" —
/// where "fallback" is not glued to a colon — still trips the guard.
fn strip_fallback_prop(source: &str) -> String {
    source.replace("fallback:", "")
}

#[test]
fn the_fallback_guard_strips_the_prop_but_keeps_real_markers() {
    // The Dioxus prop is markup, not a marker.
    assert!(!strip_fallback_prop("SuspenseBoundary { fallback: |_| rsx!{} }").contains("fallback"));
    // A genuine "fallback data" marker survives the strip.
    assert!(strip_fallback_prop("// fallback data until the API lands").contains("fallback"));
}

#[test]
fn the_guard_still_catches_a_real_marker() {
    let marker = "place".to_owned() + "holder";
    let disguised = format!("let count = 5; // {marker} until the backend lands");
    assert!(strip_placeholder_attributes(&disguised).contains(&marker));
    // …while the bare HTML attribute is not a marker.
    let attribute = format!("input {{ {marker}: \"Search…\" }}");
    assert!(!strip_placeholder_attributes(&attribute).contains(&marker));
}
