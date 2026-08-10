//! Default brand, loaded from `config/brand.yaml`.
//!
//! The YAML is embedded at compile time and parsed once into [`DEFAULTS`]. It is the
//! platform default (currently **iGEN News**); an admin may override brand name,
//! logo and colours at runtime via the workspace-branding system, but the defaults
//! here are what every unbranded surface shows. Themes (light/dark/font) are a
//! separate, per-user concern and never touch the brand.

use std::sync::LazyLock;

/// The embedded default brand config (the canonical file lives at `config/brand.yaml`).
const BRAND_YAML: &str = include_str!("../../../config/brand.yaml");

/// Resolved default brand values. Some fields (tagline, favicon, accent, …) are
/// parsed and kept for surfaces that will consume them; allow them to sit unread.
#[allow(dead_code)]
pub struct BrandDefaults {
    pub name: String,
    pub subtitle: String,
    pub tagline: String,
    pub description: String,
    pub mark: String,
    pub logo: String,
    pub favicon: String,
    pub accent: String,
    pub ink: String,
}

/// Parsed once from the embedded YAML. Read via the free accessors below.
pub static DEFAULTS: LazyLock<BrandDefaults> = LazyLock::new(|| {
    let map = parse_flat_yaml(BRAND_YAML);
    let get = |k: &str, fallback: &str| map.get(k).cloned().unwrap_or_else(|| fallback.to_owned());
    BrandDefaults {
        name: get("name", "iGEN News"),
        subtitle: get("subtitle", "Editorial Newsroom"),
        tagline: get("tagline", ""),
        description: get("description", ""),
        mark: get("mark", "iG"),
        logo: get("logo", "/assets/brand/logo.svg"),
        favicon: get("favicon", "/assets/brand/favicon.svg"),
        accent: get("accent", "#0a0a0a"),
        ink: get("ink", "#0a0a0a"),
    }
});

/// Parses a flat `key: "value"` YAML map — enough for the brand config. Ignores blank
/// lines and `#` comments; strips one layer of surrounding single or double quotes.
fn parse_flat_yaml(src: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim();
        // Drop a trailing inline comment on unquoted values.
        let mut value = value.trim();
        if !(value.starts_with('"') || value.starts_with('\'')) {
            if let Some(idx) = value.find(" #") {
                value = value[..idx].trim();
            }
        }
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if !key.is_empty() {
            out.insert(key.to_owned(), value.to_owned());
        }
    }
    out
}

// -- Ergonomic accessors (String clones; brand text appears in a handful of places). --

/// The brand name (e.g. "iGEN News").
#[must_use]
pub fn name() -> String {
    DEFAULTS.name.clone()
}

/// The brand subtitle / product line (e.g. "Editorial Newsroom").
#[must_use]
pub fn subtitle() -> String {
    DEFAULTS.subtitle.clone()
}

/// The masthead monogram mark (e.g. "iG"). Retained for text-fallback surfaces; the
/// chrome now renders the SVG logo assets instead.
#[must_use]
#[allow(dead_code)]
pub fn mark() -> String {
    DEFAULTS.mark.clone()
}
